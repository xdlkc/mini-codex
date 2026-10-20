#!/usr/bin/env python3
"""Real document-selection AI JSONL RPCs against a loopback-only Responses server.

All credentials, documents, attachments, homes and workspaces are temporary fixtures.
No inherited account, real provider, browser, or model tool is used.
"""
from __future__ import annotations

import argparse
import base64
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
from pathlib import Path
import queue
import struct
import sys
import tempfile
import threading
import time
from typing import Any
import zlib

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("document_ai_account_helpers", Path(__file__).with_name("smoke-account.py"))
assert SPEC and SPEC.loader
ACCOUNT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ACCOUNT
SPEC.loader.exec_module(ACCOUNT)
AppServer, SmokeFailure, require = ACCOUNT.AppServer, ACCOUNT.SmokeFailure, ACCOUNT.require
OLD, NEW = ACCOUNT.OLD_KEY, ACCOUNT.NEW_KEY
OAUTH_ACCESS = "e30." + base64.urlsafe_b64encode(json.dumps({"exp": 4_000_000_000}).encode()).decode().rstrip("=") + ".fixture-signature"
OAUTH_REFRESH = "unused-document-fixture-refresh"
SECRETS = ACCOUNT.ALL_KEYS + [OAUTH_ACCESS, OAUTH_REFRESH]
AI = "desktop/document/ai/"
MODEL = "local-mock-model"


def public(value: Any) -> None:
    encoded = json.dumps(value, ensure_ascii=False)
    require(all(secret not in encoded for secret in SECRETS), "a document AI RPC, notification, or error leaked a fixture credential")


@dataclass
class Plan:
    text: str = "**精简 alpha**"
    kind: str = "ready"
    key: str = OLD
    account_id: str | None = None
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)
    finished: threading.Event = field(default_factory=threading.Event)


class Provider:
    def __init__(self, timeout: float):
        self.timeout = timeout
        self.plans: queue.Queue[Plan] = queue.Queue()
        self.requests: queue.Queue[dict[str, Any]] = queue.Queue()
        self.history: list[dict[str, Any]] = []
        self.validations: list[str] = []
        self.all_plans: list[Plan] = []
        self.errors: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_: Any) -> None:
                pass

            def do_GET(self) -> None:
                try:
                    require(self.path == "/v1/models", "unexpected fixture key-validation endpoint")
                    authorization = self.headers.get("Authorization", "")
                    require(authorization in ["Bearer " + OLD, "Bearer " + NEW], "validation used an unexpected credential")
                    owner.validations.append(authorization[7:])
                    body = json.dumps({"data": [{"id": MODEL}]}).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(body)
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
                    self.close_connection = True

            def do_POST(self) -> None:
                plan = None
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path == "/v1/responses", "unexpected fixture model endpoint")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length <= 33 * 1024 * 1024, "invalid fixture request size")
                    body = json.loads(self.rfile.read(length))
                    authorization = self.headers.get("Authorization", "")
                    require(authorization in ["Bearer " + key for key in [OLD, NEW, OAUTH_ACCESS]], "suggestion used a credential outside the isolated fixtures")
                    record = {"key": authorization[7:], "body": body}
                    # Count HTTP arrival before waiting for a fixture plan. An unwanted retry
                    # must not hide behind an empty plan queue during restart assertions.
                    owner.history.append(record)
                    plan = owner.plans.get(timeout=owner.timeout)
                    require(self.headers.get("Authorization") == "Bearer " + plan.key, "suggestion used a stale or unexpected credential")
                    require(self.headers.get("chatgpt-account-id") == plan.account_id, "suggestion used the wrong OAuth account header")
                    require(body.get("model") == MODEL, "suggestion used a different model")
                    require(body.get("tools") == [] and body.get("store") is False, "selection AI enabled tools or provider history")
                    require(body.get("max_output_tokens") == 8192, "selection AI lost its provider output bound")
                    owner.requests.put(record)
                    if plan.kind == "http_error":
                        payload = ("fixture provider must not echo " + OLD).encode()
                        self.send_response(403)
                        self.send_header("Content-Type", "text/plain")
                        self.send_header("Content-Length", str(len(payload)))
                        self.send_header("Connection", "close")
                        self.end_headers()
                        self.wfile.write(payload)
                        plan.opened.set()
                        return
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if plan.kind in ["slow", "overflow"]:
                        text = "x" * (64 * 1024 + 1) if plan.kind == "overflow" else "尚未完成的本机草稿"
                        self.event({"type": "response.output_text.delta", "delta": text})
                        plan.opened.set()
                        require(plan.release.wait(owner.timeout), "gated fixture was not released before its deadline")
                    elif plan.kind == "tool":
                        self.event({"type": "response.output_item.added", "item": {"type": "function_call", "id": "forbidden-tool", "call_id": "forbidden-tool", "name": "shell", "arguments": json.dumps({"command": ["/usr/bin/touch", "should-never-be-created.txt"]})}})
                    message = {"type": "message", "id": "fixture-output", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": plan.text}]}
                    # Final-only output proves the controller does not mistake a delta buffer for a completed result.
                    self.event({"type": "response.completed", "response": {"status": "completed", "output": [message], "usage": {"input_tokens": 23, "output_tokens": 14, "total_tokens": 37}}})
                    plan.opened.set()
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Local cancellation may close the connection before the provider finishes.
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
                    if plan:
                        plan.finished.set()
                    self.close_connection = True

            def event(self, value: dict[str, Any]) -> None:
                frame = ("data: " + json.dumps(value, ensure_ascii=False) + "\r\n\r\n").encode()
                self.wfile.write(f"{len(frame):x}\r\n".encode() + frame + b"\r\n")
                self.wfile.flush()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def plan(self, text: str = "**精简 alpha**", *, kind: str = "ready", key: str = OLD, account_id: str | None = None) -> Plan:
        plan = Plan(text, kind, key, account_id)
        self.all_plans.append(plan)
        self.plans.put(plan)
        return plan

    def request(self) -> dict[str, Any]:
        try:
            return self.requests.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure("the loopback provider received no expected suggestion request") from error

    def __enter__(self) -> Provider:
        return self

    def __exit__(self, *_: Any) -> None:
        for plan in self.all_plans:
            plan.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def rpc(app: AppServer, method: str, params: dict[str, Any] | None = None) -> Any:
    result = app.rpc(method, params)
    public(result)
    public(app.notifications)
    return result


def rejected(app: AppServer, method: str, params: dict[str, Any]) -> str:
    try:
        rpc(app, method, params)
    except SmokeFailure as error:
        message = str(error)
        public(message)
        require("RPC error:" in message, "expected rejection failed through transport instead of the real RPC")
        return message
    raise SmokeFailure("a forbidden document AI operation unexpectedly succeeded")


def body() -> dict[str, Any]:
    return {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "alpha 🙂 tail"}]}, {"type": "paragraph", "content": [{"type": "text", "text": "UNSELECTED_DOCUMENT_FIXTURE"}]}]}


def create(app: AppServer, rich_body: dict[str, Any] | None = None, title: str = "UNSELECTED_TITLE_FIXTURE") -> dict[str, Any]:
    return rpc(app, "desktop/document/create", {"title": title, "format": "richtext", "schemaVersion": 1, "body": rich_body or body()})["document"]


def proposal(app: AppServer, document: dict[str, Any], *, selection: dict[str, Any] | None = None) -> dict[str, Any]:
    result = rpc(app, AI + "propose", {"id": document["id"], "expectedRevision": document["revision"], "selection": selection or {"from": 1, "to": 6, "quote": "alpha"}, "instructions": "Make the selected content concise", "model": MODEL})["proposal"]
    require(result["status"] == "queued" and result["revision"] == 1 and result["baseRevision"] == document["revision"], "propose did not immediately return a queued, versioned review job")
    require(result["proposedMarkdown"] == "" and result["stale"] is False, "queued proposal contains an unverified result or stale source")
    return result


def read_document(app: AppServer, document_id: str) -> dict[str, Any]:
    return rpc(app, "desktop/document/read", {"id": document_id})["document"]


def read_proposal(app: AppServer, proposal_id: str) -> dict[str, Any]:
    return rpc(app, AI + "read", {"proposalId": proposal_id})["proposal"]


def terminal(app: AppServer, proposal_id: str, expected: str) -> dict[str, Any]:
    def found() -> list[dict[str, Any]]:
        return [item["params"]["proposal"] for item in app.notifications if item.get("method") == AI + "updated" and item.get("params", {}).get("proposal", {}).get("proposalId") == proposal_id and item["params"]["proposal"].get("status") in ["ready", "failed", "cancelled", "interrupted"]]
    app.pump_until(lambda: bool(found()), "document AI terminal notification")
    require(found()[-1]["status"] == expected, "suggestion notification reached an unexpected terminal state")
    result = read_proposal(app, proposal_id)
    require(result["status"] == expected, "terminal suggestion state was not persisted")
    revisions = [item["params"]["proposal"]["revision"] for item in app.notifications if item.get("method") == AI + "updated" and item.get("params", {}).get("proposal", {}).get("proposalId") == proposal_id]
    require(revisions == sorted(revisions) and result["revision"] >= max(revisions), "proposal notifications moved backwards")
    return result


def settle(app: AppServer, plan: Plan, proposal_id: str, expected: str) -> None:
    plan.release.set()
    require(plan.finished.wait(app.timeout), "the late loopback provider result did not finish")
    deadline = time.monotonic() + 0.2
    while time.monotonic() < deadline:
        result = read_proposal(app, proposal_id)
        require(result["status"] == expected and result["proposedMarkdown"] == "", "a cancelled or old-account response became an accepted proposal")
        time.sleep(0.035)


def png() -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress((b"\0" + bytes([40, 160, 220, 255]) * 2) * 2)) + chunk(b"IEND", b"")


def run(binary: Path, timeout: float) -> None:
    checks, rpc_count = 0, 0
    live: list[AppServer] = []
    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print("PASS " + label, flush=True)
    def stop(app: AppServer, *, crash: bool = False) -> None:
        nonlocal rpc_count
        if app not in live:
            return
        live.remove(app)
        rpc_count += app.rpc_count
        if crash:
            app.process.kill()
            app.process.wait(timeout=2)
        app.close()
        public(list(app.stderr))

    with tempfile.TemporaryDirectory(prefix="mcodex-document-ai-smoke-") as directory:
        root = Path(directory).resolve()
        workspace, fixture_home, store = root / "workspace", root / "home", root / "own"
        workspace.mkdir(); fixture_home.mkdir(); store.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        with Provider(timeout) as provider:
            def start(path: Path = store) -> AppServer:
                app = AppServer(binary, path, workspace, fixture_home, provider.base_url, timeout)
                live.append(app)
                ACCOUNT.initialize(app, path)
                return app
            try:
                app = start()
                cap = rpc(app, AI + "capabilities")
                require(cap["available"] is False and cap["requiresReview"] is True and cap["automaticallyApplies"] is False and cap["automaticRetry"] is False, "signed-out capability overstated service behavior")
                doc = create(app)
                rejected(app, AI + "propose", {"id": doc["id"], "expectedRevision": doc["revision"], "selection": {"from": 1, "to": 6, "quote": "alpha"}, "instructions": "No provider access", "model": MODEL})
                require(not provider.history and not provider.validations, "signed-out suggestion contacted a provider")
                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": OLD})
                cap = rpc(app, AI + "capabilities")
                require(cap["available"] is True and cap["permissionsVerified"] is False and cap["defaultModel"] == MODEL and cap["selectionPositions"] == "prosemirror-utf16", "connected capability or configured model contract is wrong")
                for selection in [{"from": 1, "to": 6, "quote": "wrong"}, {"from": 7, "to": 8, "quote": ""}, {"from": 0, "to": 999, "quote": ""}]:
                    rejected(app, AI + "propose", {"id": doc["id"], "expectedRevision": doc["revision"], "selection": selection, "instructions": "Reject invalid range", "model": MODEL})
                require(not provider.history, "invalid quote or UTF16 range reached the provider")
                passed("isolated account capability, configured model and invalid UTF16/quote guards")

                before = (store / "desktop.json").read_bytes()
                provider.plan()
                queued = proposal(app, doc)
                wire = provider.request()["body"]
                require("reasoning" not in wire, "auto reasoning was not omitted")
                selected = json.dumps(wire["input"], ensure_ascii=False)
                require("alpha" in selected and "Make the selected content concise" in selected, "selection or requested change did not reach the real Responses request")
                require(all(marker not in selected for marker in ["tail", "UNSELECTED_DOCUMENT_FIXTURE", "UNSELECTED_TITLE_FIXTURE", str(store), str(workspace)]), "unselected document or local context leaked into model input")
                ready = terminal(app, queued["proposalId"], "ready")
                require(ready["proposedMarkdown"] == "**精简 alpha**" and ready["usage"]["totalTokens"] == 37 and ready["stale"] is False, "final-only output, actual usage or source version was lost")
                require(read_document(app, doc["id"]) == doc and (store / "desktop.json").read_bytes() == before, "an AI suggestion wrote document content before user review")
                accepted_body = body()
                accepted_body["content"][0]["content"] = [{"type": "text", "text": "精简 alpha", "marks": [{"type": "bold"}]}, {"type": "text", "text": " 🙂 tail"}]
                accepted = rpc(app, "desktop/document/update", {"id": doc["id"], "expectedRevision": ready["baseRevision"], "body": accepted_body})["document"]
                require(accepted["body"] == accepted_body and accepted["revision"] == doc["revision"] + 1, "ordinary document CAS did not apply the reviewed structured edit")
                require(accepted["body"]["content"][1:] == doc["body"]["content"][1:] and accepted["body"]["content"][0]["content"][-1]["text"] == " 🙂 tail", "manual acceptance lost unselected structured content")
                require(read_proposal(app, ready["proposalId"])["stale"] is True, "accepted old-base proposal was not marked stale")
                first_id, accepted_id = ready["proposalId"], accepted["id"]
                passed("real proposal notification, final-only output and usage; no automatic writes; ordinary CAS acceptance")

                stale_doc = create(app)
                plan = provider.plan("迟到的修改", kind="slow")
                pending = proposal(app, stale_doc); provider.request()
                require(plan.opened.wait(timeout), "stale-source fixture never started streaming")
                external = rpc(app, "desktop/document/update", {"id": stale_doc["id"], "expectedRevision": stale_doc["revision"], "title": "Human version two"})["document"]
                plan.release.set()
                stale = terminal(app, pending["proposalId"], "ready")
                require(stale["stale"] is True and stale["baseRevision"] == stale_doc["revision"], "a late suggestion was falsely bound to the newer document")
                before = (store / "desktop.json").read_bytes()
                conflict = rpc(app, "desktop/document/update", {"id": stale_doc["id"], "expectedRevision": stale["baseRevision"], "body": body()})
                require(conflict["conflict"]["actualRevision"] == external["revision"] and (store / "desktop.json").read_bytes() == before, "stale acceptance bypassed document CAS or rewrote persisted state")
                require(read_document(app, stale_doc["id"]) == external, "stale suggestion overwrote the human edit")
                passed("in-flight source edits preserve reviewable stale output and reject old-revision acceptance")

                image_bytes = png()
                image = rpc(app, "files/import", {"name": "selected.png", "mimeType": "image/png", "dataBase64": base64.b64encode(image_bytes).decode()})["attachment"]
                file_bytes = "ALL_SELECTED_FILE_CONTENT\n第二行不可截断".encode()
                file = rpc(app, "files/import", {"name": "selected.txt", "mimeType": "text/plain", "dataBase64": base64.b64encode(file_bytes).decode()})["attachment"]
                excluded = rpc(app, "files/import", {"name": "UNSELECTED_FILE.txt", "dataBase64": base64.b64encode(b"UNSELECTED_FILE_CONTENT").decode()})["attachment"]
                atom_body = {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "A"}, {"type": "hardBreak"}, {"type": "inlineMath", "attrs": {"latex": "x^2"}}]}, {"type": "image", "attrs": {"attachmentId": image["id"], "name": "selected.png"}}, {"type": "fileAttachment", "attrs": {"attachmentId": file["id"], "name": "selected.txt"}}, {"type": "fileAttachment", "attrs": {"attachmentId": excluded["id"], "name": "UNSELECTED_FILE.txt"}}]}
                atom_doc = create(app, atom_body)
                selection = {"from": 1, "to": 7, "quote": "A\ufffc\ufffc\n\ufffc\n\ufffc"}
                provider.plan("图片与文件建议")
                pending = proposal(app, atom_doc, selection=selection)
                wire = provider.request()["body"]
                context = wire["input"][0]["content"]
                semantic = json.loads(context[0]["text"])["selectedContent"]
                require(semantic.startswith("A\n$x^2$") and file["id"] in semantic and image["id"] in semantic, "selected breaks, LaTeX or attachment references lost semantic content")
                images = [item["image_url"] for item in context if item.get("type") == "input_image"]
                require(len(images) == 1 and base64.b64decode(images[0].split(",", 1)[1], validate=True) == image_bytes, "selected image was not loaded from the real attachment original")
                flattened = json.dumps(context, ensure_ascii=False)
                require(file_bytes.decode() in "\n".join(item.get("text", "") for item in context), "selected file contents were truncated or replaced by a label")
                require("UNSELECTED_FILE" not in flattened and excluded["id"] not in flattened, "an unselected file was sent to the provider")
                complete = terminal(app, pending["proposalId"], "ready")
                require(complete["selectionAttachmentIds"] == [image["id"], file["id"]], "selected source IDs were not persisted in order")
                rpc(app, "files/delete", {"id": file["id"]})
                count_before = len(provider.history)
                unreadable = proposal(app, atom_doc, selection=selection)
                require(terminal(app, unreadable["proposalId"], "failed")["proposedMarkdown"] == "" and len(provider.history) == count_before, "unreadable selected source was sent or falsely summarized")
                passed("real attachment originals and full file text enter selected-only multimodal context; unreadable sources fail before HTTP")

                cancel_doc = create(app)
                plan = provider.plan("不可采纳的迟到结果", kind="slow")
                pending = proposal(app, cancel_doc); provider.request()
                require(plan.opened.wait(timeout), "cancel fixture did not stream")
                cancelled = rpc(app, AI + "cancel", {"proposalId": pending["proposalId"]})["proposal"]
                require(cancelled["status"] == "cancelled" and "provider may still" in cancelled["cancelNote"], "cancellation falsely promised provider termination")
                require(rpc(app, AI + "cancel", {"proposalId": pending["proposalId"]})["proposal"]["revision"] == cancelled["revision"], "repeat cancellation mutated a terminal proposal")
                settle(app, plan, pending["proposalId"], "cancelled")
                require(read_document(app, cancel_doc["id"]) == cancel_doc, "cancelled proposal wrote the source document")
                cancel_id = pending["proposalId"]
                passed("idempotent local cancellation preserves source content and rejects late provider results")

                for kind in ["tool", "http_error", "overflow"]:
                    plan = provider.plan("must not become ready", kind=kind)
                    pending = proposal(app, cancel_doc); provider.request()
                    failed = terminal(app, pending["proposalId"], "failed")
                    require(failed["proposedMarkdown"] == "" and len(failed["error"]) < 300, "provider failure retained replacement text or an unbounded error")
                    if kind == "http_error":
                        require("HTTP 403" in failed["error"], "HTTP failure lost its safe status code")
                    if kind == "overflow":
                        require(plan.opened.is_set() and not plan.release.is_set(), "overflow waited for provider completion instead of cancelling its future")
                        settle(app, plan, pending["proposalId"], "failed")
                require(not (workspace / "should-never-be-created.txt").exists(), "selection AI executed an undeclared model tool")
                require(read_document(app, cancel_doc["id"]) == cancel_doc, "a failed request wrote document content")
                passed("added tool output cannot be hidden by final text; provider errors redact credentials; output limit cancels the actual future")

                plan = provider.plan("old-key late suggestion", kind="slow")
                pending = proposal(app, cancel_doc); require(provider.request()["key"] == OLD, "initial account was not used")
                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": NEW})
                settle(app, plan, pending["proposalId"], "interrupted")
                provider.plan("new-key reviewed suggestion", key=NEW)
                pending = proposal(app, cancel_doc); require(provider.request()["key"] == NEW, "DocumentAi cached an old key across login")
                require(terminal(app, pending["proposalId"], "ready")["proposedMarkdown"] == "new-key reviewed suggestion", "reusable shutdown disabled the new account")
                plan = provider.plan("logout late suggestion", kind="slow", key=NEW)
                pending = proposal(app, cancel_doc); provider.request()
                rpc(app, "account/logout")
                settle(app, plan, pending["proposalId"], "interrupted")
                count_before = len(provider.history)
                rejected(app, AI + "propose", {"id": cancel_doc["id"], "expectedRevision": cancel_doc["revision"], "selection": {"from": 1, "to": 6, "quote": "alpha"}, "instructions": "No cached key", "model": MODEL})
                require(rpc(app, AI + "capabilities")["available"] is False and len(provider.history) == count_before, "logout reused old credentials")
                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": NEW})
                passed("real login/logout wiring interrupts old generations and later requests use only the current Authorization")

                plan = provider.plan("first instance paid result", kind="slow", key=NEW)
                pending = proposal(app, cancel_doc); provider.request()
                second = start()
                cap = rpc(second, AI + "capabilities")
                require(cap["available"] is False and "Another app instance" in cap["reason"], "second app-server stole the running controller lease")
                require(read_proposal(second, pending["proposalId"])["status"] == "running", "second app-server recovery interrupted the first app-server's request")
                require(any(item["proposalId"] == pending["proposalId"] for item in rpc(second, AI + "list", {"id": cancel_doc["id"]})["data"]), "passive instance cannot read shared proposal history")
                rejected(second, AI + "cancel", {"proposalId": pending["proposalId"]})
                rejected(second, AI + "propose", {"id": cancel_doc["id"], "expectedRevision": cancel_doc["revision"], "selection": {"from": 1, "to": 6, "quote": "alpha"}, "instructions": "Do not steal lease", "model": MODEL})
                plan.release.set()
                require(terminal(app, pending["proposalId"], "ready")["proposedMarkdown"] == "first instance paid result", "second instance discarded the first paid response")
                stop(second)
                passed("two real app-servers share read history while only the lease owner can mutate live proposals")

                proposals = rpc(app, AI + "list")["data"]
                require(all(item["id"] for item in proposals) and any(item["proposalId"] == first_id for item in proposals), "completed proposal is absent from history")
                persisted = store / "document-ai/proposals.json"
                require(persisted.stat().st_mode & 0o777 == 0o600 and persisted.parent.stat().st_mode & 0o777 == 0o700, "proposal history is not private")
                stop(app)
                app = start()
                require(read_document(app, accepted_id)["body"] == accepted_body and read_proposal(app, first_id)["proposedMarkdown"] == "**精简 alpha**", "document acceptance or proposal history did not survive restart")
                require(read_proposal(app, cancel_id)["status"] == "cancelled", "cancelled proposal did not survive restart")
                require(len(rpc(app, AI + "list")["data"]) == len(proposals), "restart added or lost proposal records")
                passed("private proposal records, reviewed document and terminal statuses survive a real process restart")

                plan = provider.plan("EOF late suggestion", kind="slow", key=NEW)
                pending = proposal(app, cancel_doc); provider.request()
                require(plan.opened.wait(timeout), "EOF fixture did not stream")
                before_count = len(provider.history)
                assert app.process.stdin is not None
                app.process.stdin.close()
                app.process.wait(timeout=timeout)
                require(app.process.returncode == 0 and not plan.release.is_set(), "EOF failed to close the server independently of the pending provider")
                stop(app)
                plan.release.set(); require(plan.finished.wait(timeout), "EOF late response did not finish")
                app = start()
                stopped = read_proposal(app, pending["proposalId"])
                require(stopped["status"] == "interrupted" and stopped["proposedMarkdown"] == "", "EOF accepted a late suggestion or left an active job")
                require(len(provider.history) == before_count, "restart retried a potentially charged EOF request")
                require(read_document(app, cancel_doc["id"]) == cancel_doc, "EOF cleanup changed the document")
                passed("EOF cancels local waiting before provider release and restart never retries the charged request")

                plan = provider.plan("crash late suggestion", kind="slow", key=NEW)
                pending = proposal(app, cancel_doc); provider.request()
                require(plan.opened.wait(timeout), "crash fixture did not stream")
                before_count = len(provider.history)
                stop(app, crash=True)
                plan.release.set(); require(plan.finished.wait(timeout), "crash late response did not finish")
                app = start()
                recovered = read_proposal(app, pending["proposalId"])
                require(recovered["status"] == "interrupted" and recovered["proposedMarkdown"] == "" and len(provider.history) == before_count, "crash recovery accepted a late response or retried paid work")
                passed("OS lease release after a killed process permits safe interrupted recovery without duplicate requests")

                oauth_store = root / "oauth"
                oauth_store.mkdir()
                (oauth_store / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "tokens": {"access_token": OAUTH_ACCESS, "refresh_token": OAUTH_REFRESH, "account_id": "fixture-document-oauth"}}), encoding="utf-8")
                (oauth_store / "auth.json").chmod(0o600)
                oauth = start(oauth_store)
                require(rpc(oauth, AI + "capabilities")["available"] is True, "ChatGPT account was incorrectly rejected by document Responses")
                oauth_doc = create(oauth)
                provider.plan("OAuth fixture suggestion", key=OAUTH_ACCESS, account_id="fixture-document-oauth")
                pending = proposal(oauth, oauth_doc); provider.request()
                require(terminal(oauth, pending["proposalId"], "ready")["proposedMarkdown"] == "OAuth fixture suggestion", "valid, unexpired OAuth fixture did not complete through actual account headers")
                stop(oauth)
                passed("unexpired isolated ChatGPT OAuth fixture uses real Responses account headers without opening or refreshing an account")

                require(not provider.errors and provider.plans.empty() and provider.requests.empty(), "unresolved fixture or provider protocol errors remain")
                for item in live:
                    public(item.notifications)
                for path in store.rglob("*.json"):
                    if path.name != "auth.json":
                        public(path.read_text(encoding="utf-8"))
            finally:
                for app in list(live):
                    stop(app)
            print(f"PASS document AI smoke: {checks} checks, {rpc_count} RPCs, {len(provider.history)} local Responses HTTP requests, {len(provider.validations)} local key validations; real Rust documents and attachment helpers; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=8)
    options = parser.parse_args()
    require(options.binary.is_file(), "build mcodex before running this smoke")
    require(0 < options.timeout <= 10, "timeout must be at most 10 seconds")
    try:
        run(options.binary.resolve(), options.timeout)
        return 0
    except Exception as error:
        message = str(error)
        for secret in SECRETS:
            message = message.replace(secret, "[redacted fixture credential]")
        print("FAIL document AI smoke: " + message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
