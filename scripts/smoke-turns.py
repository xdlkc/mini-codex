#!/usr/bin/env python3
"""Exercise real Rust agent turns against a local mock Responses API.

Run `cargo build` first, then `python3 scripts/smoke-turns.py`.
The HTTP listener binds only to 127.0.0.1. All credentials are fixed test data;
all sessions, instructions, settings, and project data use a temporary home.
"""

from __future__ import annotations

import argparse
from collections import deque
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, Callable


class SmokeFailure(RuntimeError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SmokeFailure(message)


@dataclass
class Scenario:
    kind: str
    text: str = "你好，Rust 🙂"
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)


class MockResponses:
    def __init__(self, timeout: float):
        self.timeout = timeout
        self.plans: queue.Queue[Scenario] = queue.Queue()
        self.requests: queue.Queue[dict[str, Any]] = queue.Queue()
        self.scenarios: list[Scenario] = []
        self.errors: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_: Any) -> None:
                pass

            def do_POST(self) -> None:
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path == "/responses", f"unexpected mock path {self.path}")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size < 8_000_000, "invalid mock request size")
                    body = json.loads(self.rfile.read(size))
                    require(self.headers.get("Authorization") == "Bearer local-test-only", "request did not use the fixed local test credential")
                    require(body.get("model") == "local-mock-model", "request used an unexpected model")
                    scenario = owner.plans.get(timeout=owner.timeout)
                    owner.requests.put(body)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if scenario.kind == "failed":
                        self.event({"type": "response.failed", "response": {"status": "failed", "error": {"message": "local mock stream failure"}}})
                    elif scenario.kind in ["approval", "scope_escape"]:
                        command = ["/bin/sh", "-c", "printf unexpected > scope-guard-created.txt"] if scenario.kind == "scope_escape" else ["/usr/bin/true"]
                        arguments={"command":command}
                        if scenario.kind=='approval':arguments.update(sandbox_permissions='require_escalated',justification='Exercise an explicit additional-access request in the local fixture')
                        call = {"type": "function_call", "id": "mock-function", "call_id": f"mock-call-{id(scenario)}", "name": "shell", "arguments": json.dumps(arguments)}
                        self.event({"type": "response.output_item.done", "item": call})
                        self.event({"type": "response.completed", "response": {"status": "completed", "output": [call]}})
                    elif scenario.kind == "slow":
                        self.event({"type": "response.output_text.delta", "delta": "处理中…"})
                        scenario.opened.set()
                        require(scenario.release.wait(timeout=owner.timeout * 2), "slow mock was not released before its deadline")
                        # A user interruption intentionally closes without completion.
                    elif scenario.kind == "success":
                        delta = self.frame({"type": "response.output_text.delta", "delta": scenario.text})
                        chinese = "你".encode("utf-8")
                        split = delta.find(chinese)
                        require(split >= 0, "success fixture must contain the Chinese split marker")
                        # Split inside a UTF-8 code point and use CRLF event delimiters.
                        for chunk in [delta[:split + 1], delta[split + 1:split + 2], delta[split + 2:]]:
                            self.chunk(chunk)
                            time.sleep(0.002)
                        message = {"type": "message", "id": "mock-message", "role": "assistant", "phase": "final", "content": [{"type": "output_text", "text": scenario.text}]}
                        self.event({"type": "response.output_item.done", "item": message})
                        self.event({"type": "response.completed", "response": {"status": "completed", "output": [message]}})
                    else:
                        raise SmokeFailure(f"unknown mock scenario {scenario.kind}")
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Dropping an interrupted Rust request is expected.
                except Exception as error:
                    owner.errors.append(str(error))
                finally:
                    self.close_connection = True

            @staticmethod
            def frame(value: dict[str, Any]) -> bytes:
                return ("data: " + json.dumps(value, ensure_ascii=False) + "\r\n\r\n").encode("utf-8")

            def chunk(self, data: bytes) -> None:
                self.wfile.write(f"{len(data):x}\r\n".encode("ascii") + data + b"\r\n")
                self.wfile.flush()

            def event(self, value: dict[str, Any]) -> None:
                self.chunk(self.frame(value))

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def plan(self, kind: str, text: str = "你好，Rust 🙂") -> Scenario:
        scenario = Scenario(kind, text)
        self.scenarios.append(scenario)
        self.plans.put(scenario)
        return scenario

    def request(self) -> dict[str, Any]:
        try:
            return self.requests.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure(f"mock received no Responses request within {self.timeout:g}s; errors={self.errors}") from error

    def close(self) -> None:
        for scenario in self.scenarios:
            scenario.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def __enter__(self) -> MockResponses:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


class AppServer:
    def __init__(self, binary: Path, store: Path, workspace: Path, base_url: str, timeout: float):
        env = os.environ.copy()
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"]:
            env.pop(name, None)
        env.update({"MCODEX_HOME": str(store), "MCODEX_NO_CODEX_IMPORT": "1", "OPENAI_API_KEY": "local-test-only", "OPENAI_BASE_URL": base_url, "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost"})
        self.timeout = timeout
        self.next_id = 0
        self.rpc_count = 0
        self.replies: dict[int, dict[str, Any]] = {}
        self.notifications: list[dict[str, Any]] = []
        self.approval_requests: dict[int, dict[str, Any]] = {}
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr: deque[str] = deque(maxlen=30)
        self.process = subprocess.Popen([str(binary), "--model", "local-mock-model", "-C", str(workspace), "app-server"], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.readers = [threading.Thread(target=self._stdout, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for reader in self.readers:
            reader.start()

    def _stdout(self) -> None:
        assert self.process.stdout is not None
        try:
            for line in self.process.stdout:
                self.lines.put(line)
        finally:
            self.lines.put(None)

    def _stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self.stderr.append(line.rstrip())

    def pump_until(self, predicate: Callable[[], bool], label: str) -> None:
        deadline = time.monotonic() + self.timeout
        while not predicate():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeFailure(f"{label} exceeded {self.timeout:g}s; stderr={list(self.stderr)!r}")
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty as error:
                raise SmokeFailure(f"{label} timed out; stderr={list(self.stderr)!r}") from error
            require(line is not None, f"app-server exited during {label}; stderr={list(self.stderr)!r}")
            message = json.loads(line)
            if "id" in message:
                if "method" in message:
                    require(message["method"].endswith("/requestApproval"), "unexpected server request")
                    self.approval_requests[message["id"]] = message
                else:
                    self.replies[message["id"]] = message
            else:
                self.notifications.append(message)

    def rpc(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": request_id, "method": method, "params": params or {}}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        self.rpc_count += 1
        self.pump_until(lambda: request_id in self.replies, method)
        response = self.replies.pop(request_id)
        require("error" not in response, f"{method} RPC error: {response.get('error')}")
        return response["result"]

    def event(self, method: str, thread_id: str, turn_id: str | None = None, status: str | None = None) -> dict[str, Any]:
        def matching() -> list[dict[str, Any]]:
            return [event["params"] for event in self.notifications
                    if event.get("method") == method and event.get("params", {}).get("threadId") == thread_id
                    and (turn_id is None or event["params"].get("turnId", event["params"].get("turn", {}).get("id")) == turn_id)
                    and (status is None or event["params"].get("status", {}).get("type") == status)]
        self.pump_until(lambda: bool(matching()), f"{method} for {thread_id}")
        return matching()[-1]

    def start(self, mode: str, project_id: str) -> str:
        return self.rpc("thread/start", {"mode": mode, "projectId": project_id})["thread"]["id"]

    def approval(self, thread_id: str) -> dict[str, Any]:
        def matching() -> list[dict[str, Any]]:
            return [request for request in self.approval_requests.values() if request["params"].get("threadId") == thread_id]
        self.pump_until(lambda: bool(matching()), f"approval for {thread_id}")
        return matching()[-1]

    def respond_approval(self, request_id: int, decision: str) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": request_id, "result": {"decision": decision}}) + "\n")
        self.process.stdin.flush()

    def turn(self, thread_id: str, inputs: list[dict[str, Any]] | None = None) -> str:
        return self.rpc("turn/start", {"threadId": thread_id, "input": inputs or [{"type": "text", "text": "local smoke prompt"}], "reasoningEffort": "xhigh"})["turn"]["id"]

    def assert_chinese_completion(self, thread_id: str, turn_id: str, expected: str) -> None:
        result = self.event("turn/completed", thread_id, turn_id)
        require(result["turn"]["status"] == "completed", f"expected completed turn: {result}")
        text = "".join(event["params"]["delta"] for event in self.notifications if event.get("method") == "item/agentMessage/delta" and event["params"].get("threadId") == thread_id and event["params"].get("turnId") == turn_id)
        require(text == expected, f"SSE Chinese text was corrupted: {text!r}")
        history = self.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]["turns"]
        require(any(item.get("type") == "agentMessage" and item.get("text") == expected for turn in history for item in turn["items"]), "completed assistant text was not persisted")

    def close(self) -> None:
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=1)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)
        for reader in self.readers:
            reader.join(timeout=1)
        for stream in [self.process.stdout, self.process.stderr]:
            if stream is not None:
                stream.close()

    def __enter__(self) -> AppServer:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


def run(binary: Path, timeout: float) -> None:
    checks, requests = 0, 0
    with tempfile.TemporaryDirectory(prefix="mcodex-turn-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace = root / "mcodex", root / "workspace"
        store.mkdir()
        workspace.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        with MockResponses(timeout) as mock, AppServer(binary, store, workspace, mock.base_url, timeout) as app:
            initialized = app.rpc("initialize")
            require(initialized.get("codexImportDisabled") is True and Path(initialized["codexHome"]) == store, "binary did not confirm isolated test state; rebuild first")
            app.rpc("settings/write", {"serviceTier": "priority", "reasoningEffort": "xhigh"})
            app.rpc("desktop/settings/update", {"preferences": {"customInstructions": "SMOKE_CUSTOM_INSTRUCTIONS"}})
            project_id = app.rpc("desktop/project/create", {"name": "Local turn smoke", "mode": "work", "instructions": "SMOKE_PROJECT_约束"})["project"]["id"]
            image_url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jf2kAAAAASUVORK5CYII="
            for mode in ["chat", "work", "codex"]:
                scenario = mock.plan("success")
                thread_id = app.start(mode, project_id)
                inputs = [{"type": "text", "text": "local smoke prompt"}]
                if mode == "chat":
                    inputs.append({"type": "image", "imageUrl": image_url})
                turn_id = app.turn(thread_id, inputs)
                body = mock.request()
                requests += 1
                require(body["reasoning"]["effort"] == "xhigh", "xhigh was changed before reaching Responses API")
                require(body.get("service_tier") == "priority", "saved service tier was not sent")
                require("SMOKE_PROJECT_约束" in body["instructions"] and "SMOKE_CUSTOM_INSTRUCTIONS" in body["instructions"], "project/custom instructions did not reach the request")
                if mode == "chat":
                    require(body["tools"] == [{"type":"web_search"}], "Chat mode did not expose only its hosted automatic search tool")
                    require(any(part.get("type") == "input_image" and part.get("image_url") == image_url for part in body["input"][-1]["content"]), "image data URL did not reach input_image")
                else:
                    names = {tool.get("name") for tool in body["tools"]}
                    require({"shell", "apply_patch"} <= names, f"{mode} mode did not expose local tool definitions")
                app.assert_chinese_completion(thread_id, turn_id, scenario.text)
                checks += 1
                print(f"PASS {mode}: request tools, instructions, xhigh/tier, Unicode SSE, and persisted completion", flush=True)

            ultra_thread = app.start("codex", project_id)
            ultra_result = mock.plan("success")
            ultra_turn = app.rpc("turn/start", {"threadId":ultra_thread,"model":"local-mock-model","reasoningEffort":"ultra","serviceTier":"priority","input":[{"type":"text","text":"Ultra local request"}]})["turn"]["id"]
            ultra_body = mock.request();requests += 1
            require(ultra_body["reasoning"]["effort"]=="ultra" and ultra_body["service_tier"]=="priority","Ultra/Fast changed on the way to the provider")
            app.assert_chinese_completion(ultra_thread,ultra_turn,ultra_result.text)
            normal_result=mock.plan("success")
            normal_turn=app.rpc("turn/start", {"threadId":ultra_thread,"reasoningEffort":"low","serviceTier":None,"input":[{"type":"text","text":"Standard speed"}]})["turn"]["id"]
            normal_body=mock.request();requests += 1
            require("service_tier" not in normal_body,"Standard inherited priority instead of clearing it")
            app.assert_chinese_completion(ultra_thread,normal_turn,normal_result.text)
            thread_id = app.start("codex", project_id)
            baseline = mock.plan("success", "你好，先前轮次 🙂")
            baseline_turn = app.turn(thread_id)
            mock.request()
            requests += 1
            app.assert_chinese_completion(thread_id, baseline_turn, baseline.text)
            scenario = mock.plan("slow")
            turn_id = app.turn(thread_id)
            mock.request()
            requests += 1
            require(scenario.opened.wait(timeout=timeout), "slow HTTP stream never opened")
            app.event("item/agentMessage/delta", thread_id, turn_id)
            resumed = app.rpc("thread/resume", {"threadId": thread_id})["thread"]
            require(resumed["status"]["type"] == "active", "thread/resume replaced the live runner or lost active status")
            for restored in [resumed, app.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]]:
                require(restored["status"]["type"] == "active" and len(restored["turns"]) == 2, "live restore duplicated the current turn or lost previous history")
                live = restored["turns"][-1]
                require(live["id"] == turn_id and live["status"] == "inProgress", "live restore lost the active turn identity/status")
                require(sum(item["type"] == "userMessage" for item in live["items"]) == 1, "live restore duplicated the current user message")
                require(any(item["type"] == "agentMessage" and item["text"] == "处理中…" for item in live["items"]), "live restore lost streamed partial assistant output")
            listed = app.rpc("thread/list", {"limit": 500})["data"]
            require(next(item for item in listed if item["id"] == thread_id)["status"]["type"] == "active", "resuming a live thread removed its active runner")
            app.rpc("turn/interrupt", {"threadId": thread_id})
            interrupted = app.event("turn/completed", thread_id, turn_id)
            require(interrupted["turn"]["status"] == "interrupted", f"interrupt did not terminate the original turn: {interrupted}")
            scenario.release.set()
            app.event("thread/status/changed", thread_id, status="idle")
            resumed = app.rpc("thread/resume", {"threadId": thread_id})["thread"]
            require(resumed["status"]["type"] == "idle", "interrupted thread remained active")
            followup = mock.plan("success", "你好，中断后继续 🙂")
            followup_turn = app.turn(thread_id)
            mock.request()
            requests += 1
            app.assert_chinese_completion(thread_id, followup_turn, followup.text)
            checks += 1
            print("PASS live resume/read recover partial output without duplicate turns; interrupt and next turn complete", flush=True)

            mock.plan("approval")
            approval_thread = app.start("codex", project_id)
            approval_turn = app.turn(approval_thread)
            mock.request()
            requests += 1
            approval = app.approval(approval_thread)
            recovered = app.rpc("approval/list", {"threadId": approval_thread})["data"]
            require(recovered == [approval], "background approval could not be restored")
            pending_turn = app.rpc("thread/resume", {"threadId": approval_thread})["thread"]["turns"][-1]
            require(pending_turn["id"] == approval_turn and any(item["id"] == approval["params"]["itemId"] for item in pending_turn["items"]), "approval resume lost its tool item or duplicated the persisted current turn")
            require(sum(item["type"] == "userMessage" for item in pending_turn["items"]) == 1, "approval resume duplicated the user message")
            app.rpc("turn/interrupt", {"threadId": approval_thread})
            require(app.event("turn/completed", approval_thread, approval_turn)["turn"]["status"] == "interrupted", "approval wait prevented interrupted completion")
            require(app.event("approval/resolved", approval_thread)["requestId"] == approval["id"], "interrupted approval did not notify resolution")
            require(app.rpc("approval/list", {"threadId": approval_thread})["data"] == [], "interrupted approval remained pending")
            checks += 1
            print("PASS background approval restores with its live tool; interrupt removes approval and ends turn", flush=True)

            mock.plan("approval")
            approval_thread = app.start("codex", project_id)
            approval_turn = app.turn(approval_thread)
            mock.request()
            requests += 1
            approval = app.approval(approval_thread)
            declined = mock.plan("success", "你好，已拒绝执行 🙂")
            app.respond_approval(approval["id"], "decline")
            body = mock.request()
            requests += 1
            require(any(item.get("type") == "function_call_output" and "rejected by user" in item.get("output", "") for item in body["input"]), "declined command was executed or its result was lost")
            require(app.event("approval/resolved", approval_thread)["requestId"] == approval["id"], "approval response did not notify resolution")
            require(app.rpc("approval/list", {"threadId": approval_thread})["data"] == [], "answered approval remained pending")
            app.assert_chinese_completion(approval_thread, approval_turn, declined.text)
            checks += 1
            print("PASS declining recovered approval removes it and continues without executing the command", flush=True)

            before_paths = {path.relative_to(workspace) for path in workspace.rglob("*")}
            mock.plan("scope_escape")
            thread_id = app.start("chat", project_id)
            turn_id = app.turn(thread_id)
            body = mock.request()
            requests += 1
            require(body["tools"] == [{"type":"web_search"}], "scope guard fixture did not use hosted-search-only Chat mode")
            rejected = app.event("turn/completed", thread_id, turn_id)
            require(rejected["turn"]["status"] == "failed", "Chat executed or accepted an undeclared provider tool call")
            require("unavailable in Chat mode" in rejected["turn"]["error"]["message"], "Chat tool rejection did not preserve its scope error")
            require(app.rpc("approval/list", {"threadId": thread_id})["data"] == [], "undeclared Chat tool created a pending approval")
            require(not any(request["params"].get("threadId") == thread_id for request in app.approval_requests.values()), "undeclared Chat tool emitted an approval request")
            require(not any(event.get("params", {}).get("threadId") == thread_id and event["params"].get("item", {}).get("type") == "commandExecution" for event in app.notifications), "undeclared Chat tool emitted a commandExecution item")
            reopened = app.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]
            require(not any(item["type"] == "commandExecution" for turn in reopened["turns"] for item in turn["items"]), "undeclared Chat tool was persisted as command execution history")
            require({path.relative_to(workspace) for path in workspace.rglob("*")} == before_paths, "undeclared Chat tool created workspace files")
            checks += 1
            print("PASS Chat rejects provider shell calls without approvals, command execution, or workspace writes", flush=True)

            (workspace / "mentioned.txt").write_text("LOCAL_WORKSPACE_MENTION_SENTINEL",encoding="utf-8")
            mention_thread=app.start("codex",project_id)
            for prompt in ["Read this workspace file",""]:
                scenario=mock.plan("success","你好，文件引用已正确传入 🙂")
                mention_turn=app.turn(mention_thread,[{"type":"text","text":prompt},{"type":"mention","path":"mentioned.txt"}])
                request=mock.request();requests+=1
                last_user=[item for item in request["input"] if item.get("role")=="user"][-1]
                require("LOCAL_WORKSPACE_MENTION_SENTINEL" in json.dumps(last_user),"workspace file content did not reach the model")
                require("_mcodex_display" not in json.dumps(request),"private workspace presentation reached the model")
                app.assert_chinese_completion(mention_thread,mention_turn,scenario.text)
                history=app.rpc("thread/read",{"threadId":mention_thread,"includeTurns":True})["thread"]["turns"]
                user=[item for turn in history for item in turn["items"] if item["type"]=="userMessage"][-1]
                require("".join(part.get("text","") for part in user["content"])==prompt,"model-only file content leaked into the user bubble")
                require(user["workspaceFiles"]==[{"path":"mentioned.txt","name":"mentioned.txt"}],"workspace reference was lost from history")
            forked=app.rpc("thread/fork",{"threadId":mention_thread})["thread"]
            require(any(item.get("workspaceFiles") for turn in forked["turns"] for item in turn["items"]),"workspace reference did not survive a fork")
            checks+=1
            print("PASS workspace references keep content in model context and file cards in visible history/forks",flush=True)

            mock.plan("failed")
            thread_id = app.start("chat", project_id)
            turn_id = app.turn(thread_id)
            mock.request()
            requests += 1
            failed = app.event("turn/completed", thread_id, turn_id)
            require(failed["turn"]["status"] == "failed", f"SSE failure was reported as success: {failed}")
            require("local mock stream failure" in failed["turn"]["error"]["message"], "stream error message was lost")
            notification = app.event("error", thread_id, turn_id)
            require("local mock stream failure" in notification["error"]["message"], "client error notification was missing")
            require(mock.errors == [], f"mock handler errors: {mock.errors}")
            require(mock.plans.empty() and mock.requests.empty(), "mock requests/scenarios did not match the executed turns")
            checks += 1
            print("PASS SSE response.failed produces failed turn and error notification", flush=True)
            rpc_count = app.rpc_count
    print(f"PASS turn smoke: {checks} checks, {requests} loopback model mocks, {rpc_count} RPCs; no external model calls; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "debug" / "mcodex")
    parser.add_argument("--timeout", type=float, default=10, help="seconds allowed per RPC, notification, or mock request (default: 10)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        print(f"FAIL executable not found: {binary}; run cargo build first", file=sys.stderr)
        return 1
    try:
        run(binary, args.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, StopIteration) as error:
        print(f"FAIL turn smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
