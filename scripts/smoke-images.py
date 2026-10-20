#!/usr/bin/env python3
"""Real Rust JSONL Images jobs and attachments against a loopback-only provider.

No inherited credentials, real provider requests, OAuth login, or imagegen tools.
Run after building mcodex; --binary can point to an explicit host-target build.
"""
from __future__ import annotations

from dataclasses import dataclass, field
from email.parser import BytesParser
from email.policy import default
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import argparse
import base64
import hashlib
import importlib.util
import json
import os
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
SPEC = importlib.util.spec_from_file_location("images_account_helpers", Path(__file__).with_name("smoke-account.py"))
assert SPEC and SPEC.loader
ACCOUNT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ACCOUNT
SPEC.loader.exec_module(ACCOUNT)
AppServer, SmokeFailure, require = ACCOUNT.AppServer, ACCOUNT.SmokeFailure, ACCOUNT.require
OLD, NEW = ACCOUNT.OLD_KEY, ACCOUNT.NEW_KEY
OAUTH_ACCESS, OAUTH_REFRESH = "fixture-private-oauth-access", "fixture-private-oauth-refresh"
SECRETS = ACCOUNT.ALL_KEYS + [OAUTH_ACCESS, OAUTH_REFRESH]


def png(red: int, green: int, blue: int, alpha: int = 180) -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    rows = (b"\0" + bytes([red, green, blue, alpha]) * 2) * 2
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


PNG_A, PNG_B, PNG_EDIT, PNG_NEW, MASK = png(210, 40, 80), png(30, 90, 220), png(20, 180, 80), png(180, 70, 230), png(255, 255, 255, 0)


def public(value: Any) -> None:
    encoded = json.dumps(value, ensure_ascii=False)
    require(all(secret not in encoded for secret in SECRETS), "a credential leaked into an Images RPC or notification")


@dataclass
class Plan:
    path: str
    key: str
    images: list[bytes] = field(default_factory=lambda: [PNG_A])
    slow: bool = False
    status: int = 200
    payload: Any = None
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

            def result(self, status: int, value: Any) -> None:
                body = json.dumps(value).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()

            def do_GET(self) -> None:
                try:
                    require(self.path == "/v1/models", "unexpected model-validation endpoint")
                    authorization = self.headers.get("Authorization", "")
                    require(authorization in ["Bearer " + OLD, "Bearer " + NEW], "validation did not use an isolated fixture key")
                    owner.validations.append(authorization[7:])
                    self.result(200, {"data": []})
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
                    self.close_connection = True

            def do_POST(self) -> None:
                plan = None
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path in ["/v1/images/generations", "/v1/images/edits"], "unexpected image-provider endpoint")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length < 45 * 1024 * 1024, "invalid fixture request size")
                    raw = self.rfile.read(length)
                    plan = owner.plans.get(timeout=owner.timeout)
                    require(self.path == plan.path, "Images API used the wrong generation/edit endpoint")
                    require(self.headers.get("Authorization") == "Bearer " + plan.key, "Images API used a stale or unexpected credential")
                    content_type = self.headers.get("Content-Type", "")
                    if self.path.endswith("generations"):
                        body = json.loads(raw)
                    else:
                        require(content_type.startswith("multipart/form-data;"), "image edit did not send multipart data")
                        message = BytesParser(policy=default).parsebytes(b"MIME-Version: 1.0\r\nContent-Type: " + content_type.encode() + b"\r\n\r\n" + raw)
                        require(message.is_multipart(), "image edit body is not valid multipart MIME")
                        body = [{"name": part.get_param("name", header="content-disposition"), "filename": part.get_filename(), "mimeType": part.get_content_type(), "bytes": part.get_payload(decode=True)} for part in message.iter_parts()]
                    record = {"path": self.path, "key": plan.key, "body": body}
                    owner.history.append(record)
                    owner.requests.put(record)
                    plan.opened.set()
                    if plan.slow:
                        require(plan.release.wait(owner.timeout), "gated image fixture exceeded its local deadline")
                    value = plan.payload if plan.payload is not None else {"data": [{"b64_json": base64.b64encode(image).decode()} for image in plan.images], "usage": {"input_tokens": 11, "output_tokens": 31, "total_tokens": 42}}
                    self.result(plan.status, value)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Rust cancelled its wait; the provider may still finish.
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
                    if plan:
                        plan.finished.set()
                    self.close_connection = True

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def plan(self, *, edit: bool = False, key: str = OLD, images: list[bytes] | None = None, slow: bool = False, status: int = 200, payload: Any = None) -> Plan:
        plan = Plan("/v1/images/edits" if edit else "/v1/images/generations", key, [PNG_A] if images is None else images, slow, status, payload)
        self.all_plans.append(plan)
        self.plans.put(plan)
        return plan

    def request(self) -> dict[str, Any]:
        try:
            return self.requests.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure("the loopback image provider did not receive the expected request") from error

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
        public(str(error))
        require("RPC error:" in str(error), "an expected rejection failed through transport instead of a real RPC error")
        return str(error)
    raise SmokeFailure("a forbidden image request unexpectedly succeeded")


def terminal(app: AppServer, job_id: str, expected: str) -> dict[str, Any]:
    def found() -> list[dict[str, Any]]:
        return [event["params"]["job"] for event in app.notifications if event.get("method") == "images/job/updated" and event.get("params", {}).get("job", {}).get("id") == job_id and event["params"]["job"].get("status") in ["completed", "failed", "cancelled", "interrupted"]]
    app.pump_until(lambda: bool(found()), "image job terminal notification")
    require(found()[-1]["status"] == expected, "image job reached an unexpected terminal state")
    job = rpc(app, "images/job/read", {"id": job_id})["job"]
    require(job["status"] == expected, "image terminal state was not persisted")
    stages = [event["params"]["job"]["revision"] for event in app.notifications if event.get("method") == "images/job/updated" and event.get("params", {}).get("job", {}).get("id") == job_id]
    require(stages == sorted(stages) and job["revision"] >= max(stages), "image revisions or notifications moved backwards")
    return job


def file_bytes(app: AppServer, attachment_id: str, expected: bytes) -> dict[str, Any]:
    result = rpc(app, "files/read", {"id": attachment_id, "includeOriginal": True})
    require(base64.b64decode(result["content"]["dataBase64"], validate=True) == expected, "stored image bytes differ from the actual provider image")
    metadata = result["attachment"]
    require(metadata["kind"] == "image" and metadata["parseStatus"] == "ready" and metadata["mimeType"] == "image/png", "generated file did not pass the real attachment parser")
    require(metadata["sha256"] == hashlib.sha256(expected).hexdigest() and metadata["width"] == 2 and metadata["height"] == 2, "image checksum or decoded dimensions were not persisted")
    return result


def settle_cancel(app: AppServer, plan: Plan, job_id: str, expected: str, provider: Provider, asset_ids: set[str]) -> None:
    plan.release.set()
    require(plan.finished.wait(app.timeout), "the late loopback provider reply did not finish")
    until = time.monotonic() + 0.25
    while time.monotonic() < until:
        job = rpc(app, "images/job/read", {"id": job_id})["job"]
        require(job["status"] == expected and job["attachmentIds"] == [], "a cancelled or stale image job saved a late success")
        time.sleep(0.035)
    require({item["id"] for item in rpc(app, "files/list")["data"]} == asset_ids, "a cancelled or stale image request wrote an orphan image")
    require(not provider.errors, "the loopback image provider saw a protocol error")


def run(binary: Path, timeout: float) -> None:
    checks, count = 0, 0
    live: list[AppServer] = []
    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print("PASS " + label, flush=True)
    def stop(app: AppServer, *, crash: bool = False) -> None:
        nonlocal count
        if app not in live:
            return
        live.remove(app)
        count += app.rpc_count
        if crash:
            app.process.kill()
            app.process.wait(timeout=2)
        app.close()

    with tempfile.TemporaryDirectory(prefix="mcodex-images-smoke-") as directory:
        root = Path(directory).resolve()
        workspace, fixture_home, store = root / "workspace", root / "home", root / "own"
        workspace.mkdir(); fixture_home.mkdir()
        artifacts: dict[str, bytes] = {}
        with Provider(timeout) as provider:
            def start(path: Path = store) -> AppServer:
                app = AppServer(binary, path, workspace, fixture_home, provider.base_url, timeout)
                live.append(app)
                ACCOUNT.initialize(app, path)
                return app
            try:
                app = start()
                cap = rpc(app, "images/capabilities")
                require(cap["available"] is False and cap["requiresApiKey"] is True and cap["cancelStopsProvider"] is False, "signed-out image capability was overstated")
                require("API key" in rejected(app, "images/generate", {"prompt": "Do not contact a provider"}), "signed-out refusal lacks a usable account action")
                oauth_store = root / "oauth"
                oauth_store.mkdir()
                (oauth_store / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "tokens": {"access_token": OAUTH_ACCESS, "refresh_token": OAUTH_REFRESH, "account_id": "fixture-oauth-account"}}))
                (oauth_store / "auth.json").chmod(0o600)
                oauth = start(oauth_store)
                require(rpc(oauth, "account/read")["account"]["type"] == "chatgpt", "OAuth fixture was not loaded")
                require(rpc(oauth, "images/capabilities")["available"] is False, "OAuth was advertised as standard Images API access")
                require("OAuth" in rejected(oauth, "images/generate", {"prompt": "No OAuth provider call"}), "OAuth Images API refusal is not explicit")
                stop(oauth)
                require(not provider.history and not provider.validations, "signed-out or OAuth image refusal contacted the provider")
                passed("signed-out and OAuth accounts reject standard Images calls without provider access")

                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": OLD})
                cap = rpc(app, "images/capabilities")
                require(cap["available"] is True and cap["permissionsVerified"] is False and cap["defaultModel"] in cap["models"], "API-key capability or model contract is invalid")
                require("max" in cap["modelOptions"]["gpt-image-2.5-flare"]["qualities"] and "max" not in cap["modelOptions"]["gpt-image-2"]["qualities"], "new image quality settings leaked to an older model")
                plan = provider.plan(images=[PNG_A, PNG_B])
                queued = rpc(app, "images/generate", {"prompt": "Two fixture images", "n": 2})["job"]
                require(queued["status"] == "queued" and queued["revision"] == 1, "generation did not return an immediate queued job")
                request = provider.request()
                require(request["body"]["n"] == 2 and request["body"]["model"] == cap["defaultModel"] and request["body"]["output_format"] == "png", "Rust changed the requested image settings")
                complete = terminal(app, queued["id"], "completed")
                require(complete["usage"]["total_tokens"] == 42 and len(complete["attachmentIds"]) == 2, "real image usage or result count was not returned")
                for attachment_id, expected in zip(complete["attachmentIds"], [PNG_A, PNG_B]):
                    file_bytes(app, attachment_id, expected)
                    original = store / "attachments" / attachment_id / "original.bin"
                    require(original.read_bytes() == expected and original.stat().st_mode & 0o777 == 0o600, "the real generated attachment was not stored privately")
                    artifacts[attachment_id] = expected
                export = root / "download.png"
                export.write_bytes(base64.b64decode(rpc(app, "files/read", {"id": complete["attachmentIds"][0], "includeOriginal": True})["content"]["dataBase64"], validate=True))
                require(export.read_bytes() == PNG_A, "download payload did not preserve the original generated image")
                require(plan.finished.wait(timeout), "generation fixture did not finish")
                first_job, source = complete["id"], complete["attachmentIds"][0]
                passed("real generation creates private validated attachments, original bytes and a download payload")

                mask = rpc(app, "files/import", {"name": "mask.png", "mimeType": "image/png", "dataBase64": base64.b64encode(MASK).decode()})["attachment"]["id"]
                provider.plan(edit=True, images=[PNG_EDIT])
                edited = rpc(app, "images/edit", {"prompt": "Edit the masked region", "imageIds": [source], "maskId": mask, "quality": "xhigh"})["job"]
                request = provider.request()
                parts = request["body"]
                require([part["bytes"] for part in parts if part["name"] == "image[]"] == [PNG_A], "multipart edit did not upload the generated original")
                require([part["bytes"] for part in parts if part["name"] == "mask"] == [MASK], "multipart edit mask differs from the imported alpha PNG")
                require(next(part["bytes"] for part in parts if part["name"] == "quality") == b"xhigh", "edit quality was lost in multipart fields")
                require(all(str(store).encode() not in part["bytes"] for part in parts), "multipart editing exposed a private local path")
                edited = terminal(app, edited["id"], "completed")
                edited_id = edited["attachmentIds"][0]
                file_bytes(app, edited_id, PNG_EDIT)
                artifacts[edited_id] = PNG_EDIT
                asset_ids = set(artifacts) | {mask}
                require({item["id"] for item in rpc(app, "files/list")["data"]} == asset_ids, "generated images or mask are missing from Library")
                passed("generated attachments feed real multipart edits and alpha masks through the attachment store")

                plan = provider.plan(slow=True)
                pending = rpc(app, "images/generate", {"prompt": "Cancel this local wait"})["job"]
                provider.request()
                require(plan.opened.wait(timeout), "slow cancellation fixture did not start")
                cancelled = rpc(app, "images/job/cancel", {"id": pending["id"]})["job"]
                require(cancelled["status"] == "cancelled" and "may still finish" in cancelled["cancelNote"], "cancellation falsely promised provider termination")
                require(rpc(app, "images/job/cancel", {"id": pending["id"]})["job"]["status"] == "cancelled", "image cancellation is not idempotent")
                settle_cancel(app, plan, pending["id"], "cancelled", provider, asset_ids)
                cancelled_id = pending["id"]
                passed("cancel stops local waiting, persists an honest notice and rejects late image results")

                plan = provider.plan(slow=True)
                pending = rpc(app, "images/generate", {"prompt": "Old account in-flight image"})["job"]
                require(provider.request()["key"] == OLD, "initial image request used the wrong test account")
                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": NEW})
                settle_cancel(app, plan, pending["id"], "interrupted", provider, asset_ids)
                changed_id = pending["id"]
                provider.plan(key=NEW, images=[PNG_NEW])
                after = rpc(app, "images/generate", {"prompt": "Use the new account"})["job"]
                require(provider.request()["key"] == NEW, "Images service reused a cached old key after login")
                after = terminal(app, after["id"], "completed")
                new_id = after["attachmentIds"][0]
                file_bytes(app, new_id, PNG_NEW)
                artifacts[new_id] = PNG_NEW
                asset_ids.add(new_id)
                passed("API-key switching interrupts old work and the reusable controller uses the new Authorization")

                plan = provider.plan(key=NEW, slow=True)
                pending = rpc(app, "images/generate", {"prompt": "Logout while waiting"})["job"]
                provider.request()
                rpc(app, "account/logout")
                settle_cancel(app, plan, pending["id"], "interrupted", provider, asset_ids)
                before = len(provider.history)
                require(rpc(app, "images/capabilities")["available"] is False, "logout kept Images connected")
                rejected(app, "images/generate", {"prompt": "No old credential after logout"})
                require(len(provider.history) == before, "signed-out Images reused the old credential")
                passed("logout cancels active Images work and prevents cached-key reuse")

                stop(app)
                before = len(provider.history)
                app = start()
                history = {job["id"]: job for job in rpc(app, "images/job/list")["data"]}
                require(history[first_job]["status"] == "completed" and history[cancelled_id]["status"] == "cancelled" and history[changed_id]["status"] == "interrupted", "restart lost image job history or status")
                require(rpc(app, "images/capabilities")["available"] is False, "signed-out sentinel was lost across restart")
                for attachment_id, expected in artifacts.items():
                    file_bytes(app, attachment_id, expected)
                require(len(provider.history) == before, "restart automatically retried a historical request")
                passed("restart preserves generated bytes, job history, cancellation and signed-out state without retry")

                rpc(app, "account/login/start", {"type": "apiKey", "apiKey": NEW})
                plan = provider.plan(key=NEW, slow=True)
                pending = rpc(app, "images/generate", {"prompt": "Crash recovery fixture"})["job"]
                provider.request()
                require(rpc(app, "images/job/read", {"id": pending["id"]})["job"]["status"] == "running", "crash fixture was not actually in flight")
                stop(app, crash=True)
                plan.release.set()
                require(plan.finished.wait(timeout), "crashed image provider fixture did not finish")
                before = len(provider.history)
                app = start()
                recovered = rpc(app, "images/job/read", {"id": pending["id"]})["job"]
                require(recovered["status"] == "interrupted" and recovered["attachmentIds"] == [] and "will not retry" in recovered["error"], "crash recovery falsely completed or silently retried the image")
                require(len(provider.history) == before, "crash recovery contacted the provider automatically")
                require({item["id"] for item in rpc(app, "files/list")["data"]} == asset_ids, "crash recovery saved a late image")
                passed("a real app-server crash marks in-flight work interrupted without duplicate provider calls")

                for status, payload in [(401, {"error": {"message": NEW + " private provider diagnostic"}}), (200, {"data": [{"b64_json": "invalid base64 " + NEW}]})]:
                    provider.plan(key=NEW, status=status, payload=payload)
                    failed = rpc(app, "images/generate", {"prompt": "Provider failure fixture"})["job"]
                    provider.request()
                    failed = terminal(app, failed["id"], "failed")
                    require(failed["attachmentIds"] == [], "failed image request wrote a generated attachment")
                    public(failed)
                require({item["id"] for item in rpc(app, "files/list")["data"]} == asset_ids, "provider failures left orphan images")
                require(not provider.errors and provider.plans.empty() and provider.requests.empty(), "unresolved fixture requests or provider protocol errors remain")
                public(app.notifications)
                for path in store.rglob("*.json"):
                    if path.name != "auth.json":
                        public(path.read_text())
                passed("provider failures and invalid base64 remain failed, redact secrets and preserve Library")
            finally:
                for app in list(live):
                    stop(app)
            print(f"PASS images smoke: {checks} checks, {count} RPCs, {len(provider.history)} local Images HTTP requests, {len(provider.validations)} local key validations; real Rust attachment helpers; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=8)
    options = parser.parse_args()
    require(options.binary.is_file(), "build mcodex before running this smoke test")
    require(0 < options.timeout <= 10, "timeout must be at most 10 seconds")
    try:
        run(options.binary.resolve(), options.timeout)
        return 0
    except Exception as error:
        message = str(error)
        for secret in SECRETS:
            message = message.replace(secret, "[redacted fixture credential]")
        print("FAIL images smoke: " + message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
