#!/usr/bin/env python3
"""Verify real account JSONL RPCs without real credentials or external services.

Build first: cargo build
Run: python3 scripts/smoke-account.py
OAuth URLs are parsed only; no authorization website is opened or requested.
"""

from __future__ import annotations

from collections import deque
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, Callable
from urllib.parse import parse_qs, urlparse

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("account_turn_helpers", Path(__file__).with_name("smoke-turns.py"))
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
require, SmokeFailure = HELPERS.require, HELPERS.SmokeFailure

OLD_KEY = "sk-local-old-unused-fixture"
NEW_KEY = "sk-local-new-unused-fixture"
BAD_KEY = "sk-local-invalid-unused-fixture"
IMPORT_KEY = "sk-local-imported-unused-fixture"
ENV_KEY = "sk-local-managed-unused-fixture"
ALL_KEYS = [OLD_KEY, NEW_KEY, BAD_KEY, IMPORT_KEY, ENV_KEY]


class AppServer(HELPERS.AppServer):
    """Reuse the real RPC/event pump, with an explicitly constructed child env."""

    def __init__(self, binary: Path, store: Path, workspace: Path, fixture_home: Path, base_url: str, timeout: float, *, env_key: str | None = None, import_enabled: bool = False):
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(fixture_home),
            "MCODEX_HOME": str(store),
            "MCODEX_NO_CODEX_IMPORT": "0" if import_enabled else "1",
            "OPENAI_BASE_URL": base_url,
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
            "LANG": "en_US.UTF-8",
        }
        if env_key is not None:
            env["OPENAI_API_KEY"] = env_key
        self.timeout, self.next_id, self.rpc_count = timeout, 0, 0
        self.replies: dict[int, dict[str, Any]] = {}
        self.notifications: list[dict[str, Any]] = []
        self.approval_requests: dict[int, dict[str, Any]] = {}
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr: deque[str] = deque(maxlen=30)
        self.process = subprocess.Popen([str(binary), "--model", "local-mock-model", "-C", str(workspace), "app-server"], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.readers = [threading.Thread(target=self._stdout, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for reader in self.readers:
            reader.start()


@dataclass
class Reply:
    text: str
    slow: bool = False
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)


class Provider:
    def __init__(self, timeout: float):
        self.timeout = timeout
        self.posts: queue.Queue[dict[str, Any]] = queue.Queue()
        self.plans: queue.Queue[Reply] = queue.Queue()
        self.history: list[dict[str, Any]] = []
        self.validations: list[str] = []
        self.replies: list[Reply] = []
        self.errors: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_: Any) -> None:
                pass

            def key(self) -> str:
                value = self.headers.get("Authorization", "")
                require(value.startswith("Bearer "), "mock provider did not receive bearer auth")
                return value[7:]

            def do_GET(self) -> None:
                try:
                    require(self.path == "/v1/models", "unexpected validation endpoint")
                    key = self.key()
                    owner.validations.append(key)
                    valid = key in [OLD_KEY, NEW_KEY, IMPORT_KEY, ENV_KEY]
                    # Deliberately echo invalid data here. The RPC must not reflect it.
                    body = json.dumps({"data": []} if valid else {"error": {"message": "mock rejected " + key}}).encode()
                    self.send_response(200 if valid else 401)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.send_header("Connection", "close")
                    self.end_headers()
                    self.wfile.write(body)
                    self.wfile.flush()
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
                    self.close_connection = True

            def do_POST(self) -> None:
                try:
                    require(self.path == "/v1/responses", "unexpected model endpoint")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 < length < 8_000_000, "invalid mock model request size")
                    body = json.loads(self.rfile.read(length))
                    require(body.get("model") == "local-mock-model", "unexpected mock model")
                    record = {"key": self.key(), "body": body}
                    owner.history.append(record)
                    owner.posts.put(record)
                    plan = owner.plans.get(timeout=owner.timeout)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if plan.slow:
                        self.event({"type": "response.output_text.delta", "delta": "本地账户退出测试处理中…"})
                        plan.opened.set()
                        require(plan.release.wait(owner.timeout), "slow account fixture was not released")
                    else:
                        self.event({"type": "response.output_text.delta", "delta": plan.text})
                    message = {"type": "message", "id": f"fixture-message-{id(plan)}", "role": "assistant", "phase": "final", "content": [{"type": "output_text", "text": plan.text}]}
                    self.event({"type": "response.output_item.done", "item": message})
                    self.event({"type": "response.completed", "response": {"status": "completed", "output": [message], "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5}}})
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                finally:
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

    def plan(self, text: str, slow: bool = False) -> Reply:
        reply = Reply(text, slow)
        self.replies.append(reply)
        self.plans.put(reply)
        return reply

    def request(self) -> dict[str, Any]:
        try:
            return self.posts.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure("mock provider did not receive the expected local model request") from error

    def __enter__(self) -> Provider:
        return self

    def __exit__(self, *_: Any) -> None:
        for reply in self.replies:
            reply.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def assert_public(value: Any) -> None:
    text = json.dumps(value, ensure_ascii=False)
    require(all(secret not in text for secret in ALL_KEYS), "a credential leaked into public metadata or an RPC notification")


def expected_error(app: AppServer, method: str, params: dict[str, Any]) -> str:
    try:
        app.rpc(method, params)
    except SmokeFailure as error:
        text = str(error)
        assert_public(text)
        require("RPC error:" in text, "operation failed through a transport error rather than a real RPC error")
        return text
    raise SmokeFailure("an account operation that should fail unexpectedly succeeded")


def poll(app: AppServer, read: Callable[[], Any], accept: Callable[[Any], bool], label: str) -> Any:
    deadline = time.monotonic() + app.timeout
    while time.monotonic() < deadline:
        value = read()
        if accept(value):
            return value
        time.sleep(0.04)
    raise SmokeFailure(label + " exceeded its local deadline")


def initialize(app: AppServer, store: Path, imported: bool = False) -> None:
    result = app.rpc("initialize")
    require(Path(result["codexHome"]) == store and result.get("codexImportDisabled") is (not imported), "app-server did not confirm isolated authentication paths")


def write_key(store: Path, key: str) -> None:
    store.mkdir(parents=True, exist_ok=True)
    path = store / "auth.json"
    path.write_text(json.dumps({"auth_mode": "apikey", "OPENAI_API_KEY": key}), encoding="utf-8")
    path.chmod(0o600)


def assert_no_new_model_requests(app: AppServer, provider: Provider, before: int) -> None:
    deadline = time.monotonic() + 0.65
    while time.monotonic() < deadline:
        app.rpc("account/read")
        require(len(provider.history) == before, "signed-out work reused a cached credential or contacted the model")
        time.sleep(0.04)


def run(binary: Path, timeout: float) -> None:
    checks, rpc_count = 0, 0

    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print("PASS " + label, flush=True)

    with tempfile.TemporaryDirectory(prefix="mcodex-account-smoke-") as directory:
        root = Path(directory).resolve()
        workspace, fixture_home, store = root / "workspace", root / "fixture-home", root / "own"
        workspace.mkdir()
        fixture_home.mkdir()
        write_key(store, OLD_KEY)
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        with Provider(timeout) as provider:
            app = AppServer(binary, store, workspace, fixture_home, provider.base_url, timeout)
            try:
                initialize(app, store)
                account = app.rpc("account/read")
                require(account["account"]["type"] == "apiKey" and account["credentialSource"] == "local" and account["canChangeCredentials"] is True, "owned credential metadata did not match the isolated fixture")
                assert_public(account)
                original = (store / "auth.json").read_bytes()
                error = expected_error(app, "account/login/start", {"type": "apiKey", "apiKey": BAD_KEY})
                require("401" in error and (store / "auth.json").read_bytes() == original, "failed key validation changed the previous credential")
                require(provider.validations == [BAD_KEY], "API-key login did not validate against the configured loopback provider")
                passed("failed API-key validation preserves the original file and redacts provider error bodies")

                thread_id = app.rpc("thread/start", {"mode": "codex"})["thread"]["id"]
                first = provider.plan("你好，旧凭据本地验证成功")
                first_turn = app.turn(thread_id)
                request = provider.request()
                require(request["key"] == OLD_KEY, "existing chat did not start with its owned credential")
                app.assert_chinese_completion(thread_id, first_turn, first.text)
                result = app.rpc("account/login/start", {"type": "apiKey", "apiKey": NEW_KEY})
                assert_public(result)
                require(result["account"]["type"] == "apiKey" and provider.validations[-1] == NEW_KEY, "valid API-key login failed to validate or update the account")
                require(json.loads((store / "auth.json").read_text())["OPENAI_API_KEY"] == NEW_KEY, "successful validation did not save the new owned key")
                require((store / "auth.json").stat().st_mode & 0o077 == 0, "owned credentials are not private")
                second = provider.plan("你好，同一会话已使用切换后的凭据")
                second_turn = app.turn(thread_id)
                require(provider.request()["key"] == NEW_KEY, "already-loaded chat reused the previous key after account switch")
                app.assert_chinese_completion(thread_id, second_turn, second.text)
                require(any(event.get("method") == "account/updated" for event in app.notifications), "successful sign-in emitted no account update")
                assert_public(app.notifications)
                passed("validated private API-key save, safe metadata/notifications, and loaded-chat credential switching")

                slow = provider.plan("This late stream must never restore signed-out work", slow=True)
                app.rpc("thread/goal/set", {"threadId": thread_id, "objective": "ACCOUNT_LOGOUT_GOAL wait for the local slow operation", "status": "active"})
                require(provider.request()["key"] == NEW_KEY, "active Goal did not use the current account")
                require(slow.opened.wait(timeout), "real Goal SSE did not start")
                active = poll(app, lambda: app.rpc("thread/goal/get", {"threadId": thread_id})["goal"], lambda goal: goal is not None and goal["inFlight"], "active Goal")
                logout = app.rpc("account/logout")
                assert_public(logout)
                require(logout["account"] is None, "logout still reported an active local account")
                completed = app.event("turn/completed", thread_id, active["turnId"])
                require(completed["turn"]["status"] == "interrupted", "logout did not stop the actual in-flight model turn")
                paused = app.rpc("thread/goal/get", {"threadId": thread_id})["goal"]
                require(paused["status"] == "paused" and not paused["inFlight"], "logout left its Goal active")
                slow.release.set()
                count_before = len(provider.history)
                try:
                    signed_out_turn = app.turn(thread_id)
                except SmokeFailure as failure:
                    assert_public(str(failure))
                    require("RPC error:" in str(failure), "signed-out turn failed through a transport error")
                else:
                    failed = app.event("turn/completed", thread_id, signed_out_turn)
                    require(failed["turn"]["status"] == "failed", "signed-out turn incorrectly completed")
                assert_no_new_model_requests(app, provider, count_before)
                require(app.rpc("account/read")["account"] is None, "late cancelled work restored local credentials")
                assert_public(app.notifications)
                passed("logout stops slow SSE, pauses an active Goal, and blocks reuse of an old loaded-client key")

                before_cancel = (store / "auth.json").read_bytes()
                validations, posts = len(provider.validations), len(provider.history)
                start = app.rpc("account/login/start", {"type": "chatgpt"})
                require(start.get("type") == "chatgpt" and start.get("loginId"), "OAuth did not return a public pending login")
                parsed = urlparse(start["authUrl"])
                require(parsed.scheme == "https" and parsed.hostname == "auth.openai.com", "OAuth returned an unexpected authorization origin")
                query = parse_qs(parsed.query)
                redirect = urlparse(query["redirect_uri"][0])
                require(redirect.hostname == "localhost" and redirect.path == "/auth/callback" and redirect.port is not None, "OAuth callback is not a loopback endpoint")
                # Do not request authUrl. Touch only the local listener, then cancel.
                with socket.create_connection(("127.0.0.1", redirect.port), timeout=1):
                    pass
                result = app.rpc("account/login/cancel", {"loginId": start["loginId"]})
                require(result.get("cancelled") is True, "OAuth cancellation did not cancel the pending login")
                app.pump_until(lambda: any(event.get("method") == "account/login/completed" and event.get("params", {}).get("loginId") == start["loginId"] for event in app.notifications), "OAuth cancellation notification")
                notice = next(event["params"] for event in app.notifications if event.get("method") == "account/login/completed" and event["params"].get("loginId") == start["loginId"])
                require(notice.get("success") is False and notice.get("cancelled") is True, "OAuth cancellation was reported as a login error")
                deadline = time.monotonic() + timeout
                released = False
                while time.monotonic() < deadline:
                    try:
                        with socket.create_connection(("127.0.0.1", redirect.port), timeout=0.3):
                            pass
                    except OSError:
                        released = True
                        break
                    time.sleep(0.04)
                require(released, "cancelled OAuth did not release its callback listener")
                require((store / "auth.json").read_bytes() == before_cancel and len(provider.validations) == validations and len(provider.history) == posts, "OAuth cancellation changed credentials or contacted a provider")
                passed("OAuth starts and cancels through RPC, releases its listener, and never opens a provider website")
                rpc_count += app.rpc_count
            finally:
                app.close()

            imported_store = root / "import-own"
            imported_store.mkdir()
            official = fixture_home / ".codex" / "auth.json"
            official.parent.mkdir()
            official.write_text(json.dumps({"auth_mode": "apikey", "OPENAI_API_KEY": IMPORT_KEY}), encoding="utf-8")
            official.chmod(0o600)
            official_bytes = official.read_bytes()
            app = AppServer(binary, imported_store, workspace, fixture_home, provider.base_url, timeout, import_enabled=True)
            try:
                initialize(app, imported_store, imported=True)
                imported = app.rpc("account/read")
                require(imported["credentialSource"] == "imported" and imported["account"]["type"] == "apiKey", "fixture official credentials were not imported read-only")
                assert_public(imported)
                app.rpc("account/logout")
                require(official.read_bytes() == official_bytes, "logout modified the fixture official credential file")
                require(app.rpc("account/read")["account"] is None, "logout immediately reimported the official account")
                rpc_count += app.rpc_count
            finally:
                app.close()
            app = AppServer(binary, imported_store, workspace, fixture_home, provider.base_url, timeout, import_enabled=True)
            try:
                initialize(app, imported_store, imported=True)
                require(app.rpc("account/read")["account"] is None, "restart ignored signed_out and reimported the official account")
                require(official.read_bytes() == official_bytes, "restart modified the read-only fixture official file")
                assert_public(app.notifications)
                rpc_count += app.rpc_count
            finally:
                app.close()
            passed("official import stays read-only and owned signed_out survives restart without reimport")

            managed_store = root / "managed-own"
            write_key(managed_store, OLD_KEY)
            managed_bytes = (managed_store / "auth.json").read_bytes()
            app = AppServer(binary, managed_store, workspace, fixture_home, provider.base_url, timeout, env_key=ENV_KEY)
            try:
                initialize(app, managed_store)
                metadata = app.rpc("account/read")
                require(metadata["credentialSource"] == "environment" and metadata["canChangeCredentials"] is False, "environment credential priority was not reported")
                assert_public(metadata)
                validation_count = len(provider.validations)
                for method, params in [("account/login/start", {"type": "apiKey", "apiKey": NEW_KEY}), ("account/login/start", {"type": "chatgpt"}), ("account/logout", {})]:
                    require("OPENAI_API_KEY" in expected_error(app, method, params), "managed credential refusal lacked a usable explanation")
                require((managed_store / "auth.json").read_bytes() == managed_bytes and len(provider.validations) == validation_count, "environment-managed UI operations changed local credentials or called validation")
                assert_public(app.notifications)
                require(not provider.errors and provider.plans.empty() and provider.posts.empty(), "unresolved local provider work or mock protocol failures remain")
                rpc_count += app.rpc_count
            finally:
                app.close()
            passed("environment-managed credentials reject UI changes without modifying the owned file")
    print(f"PASS account smoke: {checks} checks, {rpc_count} RPCs, {len(provider.validations)} local key validations, {len(provider.history)} local Responses requests; no real account/provider login; temporary data removed", flush=True)


def main() -> int:
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=8)
    options = parser.parse_args()
    require(options.binary.is_file(), "build target/debug/mcodex first")
    require(0 < options.timeout <= 10, "timeout must be greater than 0 and at most 10 seconds")
    try:
        run(options.binary.resolve(), options.timeout)
        return 0
    except Exception as error:
        print(f"FAIL account smoke: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
