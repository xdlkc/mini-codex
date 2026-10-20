#!/usr/bin/env python3
"""Exercise the real Rust JSONL app-server without calling a model.

Build first with `cargo build`, then run `python3 scripts/smoke-desktop.py`.
All product data lives in a TemporaryDirectory. The initialization handshake
must confirm that official Codex session/auth imports are disabled.
"""

from __future__ import annotations

import argparse
from collections import deque
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any
import uuid


class SmokeFailure(RuntimeError):
    pass


class RpcFailure(SmokeFailure):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SmokeFailure(message)


class AppServer:
    """Read stdout continuously so notifications and batched replies cannot block it."""

    def __init__(self, binary: Path, store: Path, workspace: Path, timeout: float):
        env = os.environ.copy()
        env["MCODEX_HOME"] = str(store)
        env["MCODEX_NO_CODEX_IMPORT"] = "1"
        env["OPENAI_BASE_URL"] = "http://127.0.0.1:1"
        env.pop("OPENAI_API_KEY", None)
        self.timeout = timeout
        self.next_id = 0
        self.rpc_count = 0
        self.pending: dict[int, tuple[str, float]] = {}
        self.replies: dict[int, dict[str, Any]] = {}
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr: deque[str] = deque(maxlen=40)
        self.process = subprocess.Popen(
            [str(binary), "--model", "smoke-test-no-model", "-C", str(workspace), "app-server"],
            cwd=workspace,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
        )
        self.readers = [
            threading.Thread(target=self._read_stdout, daemon=True),
            threading.Thread(target=self._read_stderr, daemon=True),
        ]
        for reader in self.readers:
            reader.start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        try:
            for line in self.process.stdout:
                self.lines.put(line)
        finally:
            self.lines.put(None)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            self.stderr.append(line.rstrip())

    def diagnostics(self) -> str:
        return "\n".join(self.stderr) or "(no stderr)"

    def send(self, method: str, params: dict[str, Any] | None = None, *, timeout: float | None = None) -> int:
        require(self.process.poll() is None, f"app-server exited before {method}: {self.diagnostics()}")
        self.next_id += 1
        request_id = self.next_id
        self.pending[request_id] = (method, time.monotonic() + (self.timeout if timeout is None else timeout))
        assert self.process.stdin is not None
        try:
            self.process.stdin.write(json.dumps({"id": request_id, "method": method, "params": params or {}}, ensure_ascii=False) + "\n")
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise SmokeFailure(f"Could not send {method}: {error}\n{self.diagnostics()}") from error
        self.rpc_count += 1
        return request_id

    def wait(self, request_id: int) -> dict[str, Any]:
        method, deadline = self.pending[request_id]
        while request_id not in self.replies:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SmokeFailure(f"{method} (id {request_id}) exceeded {self.timeout:g}s\n{self.diagnostics()}")
            try:
                line = self.lines.get(timeout=remaining)
            except queue.Empty as error:
                raise SmokeFailure(f"{method} (id {request_id}) timed out\n{self.diagnostics()}") from error
            if line is None:
                raise SmokeFailure(f"app-server stdout closed during {method}\n{self.diagnostics()}")
            try:
                message = json.loads(line)
            except json.JSONDecodeError as error:
                raise SmokeFailure(f"app-server emitted invalid JSONL during {method}: {line[:300]!r}") from error
            if "id" not in message:
                continue  # Notifications are expected between RPC replies.
            if "method" in message:
                raise SmokeFailure(f"Unexpected server request {message['method']}; smoke tests never request model/tool execution")
            self.replies[message["id"]] = message
        response = self.replies.pop(request_id)
        del self.pending[request_id]
        if "error" in response:
            raise RpcFailure(f"{method}: {json.dumps(response['error'], ensure_ascii=False)}")
        require("result" in response, f"{method} returned neither result nor error")
        return response["result"]

    def rpc(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        return self.wait(self.send(method, params))

    def expect_error(self, method: str, params: dict[str, Any]) -> None:
        try:
            self.rpc(method, params)
        except RpcFailure:
            return
        raise SmokeFailure(f"{method} unexpectedly accepted {params!r}")

    def initialize(self, store: Path) -> None:
        result = self.rpc("initialize", {"clientInfo": {"name": "smoke-desktop", "version": "1"}})
        require(Path(result.get("codexHome", "")) == store, "app-server did not use the temporary MCODEX_HOME")
        require(result.get("codexImportDisabled") is True,
                "initialize must confirm codexImportDisabled=true; rebuild a binary that supports MCODEX_NO_CODEX_IMPORT before running this test")

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


def item_ids(result: dict[str, Any], key: str = "items") -> set[str]:
    return {item["id"] for item in result[key]}


def thread_ids(server: AppServer, **filters: Any) -> set[str]:
    return item_ids(server.rpc("thread/list", {"limit": 500, **filters}), "data")


def user_texts(thread: dict[str, Any]) -> list[str]:
    return [part["text"]
            for turn in thread["turns"]
            for item in turn["items"] if item["type"] == "userMessage"
            for part in item["content"] if part["type"] == "text"]


def write_history_fixture(store: Path, workspace: Path) -> tuple[str, bytes]:
    session_id = "smoke-source-" + uuid.uuid4().hex
    rows = [{"meta": {"id": session_id, "cwd": str(workspace), "model": "smoke-test-no-model", "created_ms": int(time.time() * 1000)}}]
    for role, text in [("user", "第一条 smoke"), ("assistant", "第一条回答"), ("user", "第二条 smoke"), ("assistant", "第二条回答")]:
        rows.append({"item": {"type": "message", "role": role, "content": [{"type": "input_text" if role == "user" else "output_text", "text": text}]}})
    data = ("\n".join(json.dumps(row, ensure_ascii=False) for row in rows) + "\n").encode("utf-8")
    path = store / "sessions" / f"{session_id}.jsonl"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return session_id, data


def run(binary: Path, timeout: float) -> None:
    rpc_count = 0
    checks = 0

    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print(f"PASS {label}", flush=True)

    with tempfile.TemporaryDirectory(prefix="mcodex-desktop-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace = root / "mcodex", root / "workspace"
        store.mkdir()
        workspace.mkdir()
        (store / "config.toml").write_text('model = "smoke-test-no-model"\n', encoding="utf-8")

        with AppServer(binary, store, workspace, timeout) as server:
            server.initialize(store)
            state = server.rpc("desktop/state/read")
            require(state["projects"] == [] and state["documents"] == [] and state["threadMetadata"] == [], "temporary desktop state was not empty")
            require(thread_ids(server) == set(), "official/user sessions leaked into the isolated test")
            server.rpc("desktop/settings/update", {"mode": "chat", "preferences": {"theme": "dark", "sidebarWidth": 284, "sidebarCollapsed": True}})
            passed("initialize, import isolation, and desktop settings")

            project = server.rpc("desktop/project/create", {"name": "Smoke 工作项目", "mode": "work", "instructions": "Initial instructions"})["project"]
            project_id = project["id"]
            other_project = server.rpc("desktop/project/create", {"name": "Other Codex project", "mode": "codex"})["project"]
            server.rpc("desktop/project/update", {"id": project_id, "name": "Smoke 项目已更新", "instructions": "持久化 instructions"})
            require(server.rpc("desktop/project/read", {"id": project_id})["project"]["instructions"] == "持久化 instructions", "project update was not readable")
            require(item_ids(server.rpc("desktop/project/list", {"mode": "work", "search": "已更新"})) == {project_id}, "project filters did not match updated state")
            document = server.rpc("desktop/document/create", {"title": "工作文档", "content": "# Draft\n初稿", "kind": "document", "projectId": project_id})["document"]
            document_id = document["id"]
            server.rpc("desktop/document/update", {"id": document_id, "title": "工作文档已更新", "content": "# Smoke document\n重启后应保留。"})
            code_document = server.rpc("desktop/document/create", {"title": "Example.rs", "content": 'fn main() { println!("smoke"); }', "kind": "code", "projectId": project_id})["document"]
            require(item_ids(server.rpc("desktop/document/list", {"kind": "document", "projectId": project_id, "search": "重启后"})) == {document_id}, "document kind/project/search filters failed")
            require(server.rpc("desktop/document/read", {"id": document_id})["document"]["title"] == "工作文档已更新", "document update was not readable")
            passed("desktop project/document create, read, update, and filters")

            modes: dict[str, str] = {}
            for mode in ["chat", "work", "codex"]:
                thread = server.rpc("thread/start", {"cwd": str(workspace), "mode": mode, "projectId": project_id})["thread"]
                thread_id = thread["id"]
                modes[mode] = thread_id
                require(thread["mode"] == mode and thread["projectId"] == project_id, f"thread/start lost {mode} mode/project metadata")
                server.rpc("desktop/threadMetadata/set", {"threadId": thread_id, "pinned": mode == "work", "unread": mode == "chat"})
                filtered = server.rpc("thread/list", {"mode": mode, "limit": 500})["data"]
                require({item["id"] for item in filtered} == {thread_id}, f"thread mode filter leaked another mode: {mode}")
                require(filtered[0]["pinned"] == (mode == "work"), "thread/list did not merge desktop metadata")
            metadata = server.rpc("desktop/threadMetadata/list", {"mode": "work", "pinned": True, "projectId": project_id})["items"]
            require({item["threadId"] for item in metadata} == {modes["work"]}, "metadata filters did not match the work thread")
            server.rpc("thread/name/set", {"threadId": modes["chat"], "name": "重命名 smoke"})
            require(server.rpc("thread/read", {"threadId": modes["chat"]})["thread"]["name"] == "重命名 smoke", "thread rename was not readable")
            server.rpc("thread/archive", {"threadId": modes["codex"]})
            require(modes["codex"] not in thread_ids(server), "archived thread remained in active list")
            require(modes["codex"] in thread_ids(server, archived=True), "archived thread missing from archived list")
            require(server.rpc("thread/read", {"threadId": modes["codex"]})["thread"].get("archived") is True, "thread/read disagreed with archived list")
            server.rpc("thread/unarchive", {"threadId": modes["codex"]})
            require(modes["codex"] in thread_ids(server) and modes["codex"] not in thread_ids(server, archived=True), "unarchive did not restore the active thread")
            server.rpc("thread/archive", {"threadId": modes["codex"]})
            passed("three thread modes, metadata, rename, archive, and unarchive")

            before = set((store / "sessions").glob("*.jsonl"))
            # This checks persistence under overlap, not a 100-request latency
            # SLA. Desktop mutations serialize durable file+directory syncs.
            # The observed batch reached 10.2s on this host, just beyond the
            # ordinary 10s single-RPC budget; use the UI's 30s request budget.
            pending = [server.send("thread/start", {"cwd": str(workspace), "mode": ["chat", "work", "codex"][index % 3], "projectId": project_id}, timeout=max(timeout, 30)) for index in range(100)]
            rapid_ids = [server.wait(request_id)["thread"]["id"] for request_id in pending]
            require(len(set(rapid_ids)) == 100, "rapid thread creation reused IDs")
            created = set((store / "sessions").glob("*.jsonl")) - before
            require(len(created) == 100, "rapid thread creation overwrote or lost session files")
            for session_id in rapid_ids:
                path = store / "sessions" / f"{session_id}.jsonl"
                require(path.exists(), f"missing rapid session file {session_id}")
                require(json.loads(path.read_text(encoding="utf-8").splitlines()[0])["meta"]["id"] == session_id, "session ID/file contents differ")
            require(set(rapid_ids) <= thread_ids(server), "thread/list lost rapid sessions")
            passed("100 overlapping thread/start requests preserve every session")

            source_id, source_bytes = write_history_fixture(store, workspace)
            server.rpc("desktop/threadMetadata/set", {"threadId": source_id, "mode": "work", "projectId": project_id})
            require(user_texts(server.rpc("thread/read", {"threadId": source_id, "includeTurns": True})["thread"]) == ["第一条 smoke", "第二条 smoke"], "history fixture did not load")
            forks: dict[str, str] = {}
            for label, params, expected in [
                ("full", {}, ["第一条 smoke", "第二条 smoke"]),
                ("empty", {"beforeUserMessage": 0}, []),
                ("prefix", {"beforeUserMessage": 1}, ["第一条 smoke"]),
            ]:
                fork = server.rpc("thread/fork", {"threadId": source_id, **params})["thread"]
                forks[label] = fork["id"]
                require(fork["id"] != source_id and fork["mode"] == "work" and fork["projectId"] == project_id, "fork lost identity/mode/project")
                require(user_texts(fork) == expected, f"fork {label} retained the wrong history")
                require(user_texts(server.rpc("thread/read", {"threadId": fork["id"], "includeTurns": True})["thread"]) == expected, f"fork {label} history did not persist")
            server.expect_error("thread/fork", {"threadId": source_id, "beforeUserMessage": 2})
            require((store / "sessions" / f"{source_id}.jsonl").read_bytes() == source_bytes, "fork modified the source rollout")
            passed("full and message-boundary forks, including an empty branch")
            rpc_count += server.rpc_count

        with AppServer(binary, store, workspace, timeout) as server:
            server.initialize(store)
            settings = server.rpc("desktop/settings/read")["settings"]
            require(settings["mode"] == "chat" and settings["preferences"]["theme"] == "dark" and settings["preferences"]["sidebarWidth"] == 284, "desktop settings did not survive restart")
            require(server.rpc("desktop/project/read", {"id": project_id})["project"]["name"] == "Smoke 项目已更新", "project did not survive restart")
            require(server.rpc("desktop/document/read", {"id": document_id})["document"]["content"] == "# Smoke document\n重启后应保留。", "document did not survive restart")
            metadata = server.rpc("desktop/threadMetadata/read", {"threadId": modes["work"]})["metadata"]
            require(metadata["mode"] == "work" and metadata["pinned"] and metadata["projectId"] == project_id, "thread metadata did not survive restart")
            require(server.rpc("thread/read", {"threadId": modes["chat"]})["thread"]["name"] == "重命名 smoke", "thread rename did not survive restart")
            require(modes["codex"] in thread_ids(server, archived=True) and modes["codex"] not in thread_ids(server), "archive state did not survive restart")
            require(user_texts(server.rpc("thread/read", {"threadId": forks["prefix"], "includeTurns": True})["thread"]) == ["第一条 smoke"], "fork did not survive restart")
            require(set(rapid_ids) <= thread_ids(server), "rapid sessions did not survive restart")
            passed("projects, documents, preferences, metadata, archives, and forks survive restart")

            server.rpc("thread/unarchive", {"threadId": modes["codex"]})
            server.rpc("thread/delete", {"threadId": modes["codex"]})
            require(not (store / "sessions" / f"{modes['codex']}.jsonl").exists(), "thread/delete left its native rollout")
            require(modes["codex"] not in thread_ids(server) | thread_ids(server, archived=True), "deleted thread remained in a thread list")
            server.expect_error("thread/read", {"threadId": modes["codex"]})
            server.rpc("desktop/document/delete", {"id": document_id})
            server.expect_error("desktop/document/read", {"id": document_id})
            server.rpc("desktop/project/delete", {"id": project_id})
            server.expect_error("desktop/project/read", {"id": project_id})
            require(server.rpc("desktop/document/read", {"id": code_document["id"]})["document"]["projectId"] is None, "project deletion left a dangling document assignment")
            require(server.rpc("desktop/threadMetadata/read", {"threadId": modes["work"]})["metadata"]["projectId"] is None, "project deletion left a dangling thread assignment")
            server.rpc("desktop/document/delete", {"id": code_document["id"]})
            server.rpc("desktop/project/delete", {"id": other_project["id"]})
            passed("thread/document/project deletion and assignment cleanup")
            rpc_count += server.rpc_count

        with AppServer(binary, store, workspace, timeout) as server:
            server.initialize(store)
            state = server.rpc("desktop/state/read")
            require(state["projects"] == [] and state["documents"] == [], "deleted desktop records reappeared after restart")
            require(modes["codex"] not in thread_ids(server) | thread_ids(server, archived=True), "deleted thread reappeared after restart")
            require(state["settings"]["preferences"]["sidebarCollapsed"] is True, "unchanged preferences were lost")
            passed("deletions remain deleted after a second restart")
            rpc_count += server.rpc_count

    print(f"PASS desktop smoke: {checks} checks, {rpc_count} RPCs; no model calls; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "debug" / "mcodex")
    parser.add_argument("--timeout", type=float, default=10, help="seconds allowed for each RPC response (default: 10)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        print(f"FAIL executable not found: {binary}; run cargo build first", file=sys.stderr)
        return 1
    try:
        run(binary, args.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError) as error:
        print(f"FAIL desktop smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
