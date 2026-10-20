#!/usr/bin/env python3
"""Test real remote/* dispatch through a fake SSH transport and a second Rust process.

No real host, SSH key, account, or provider is used. The fake ssh executable only
stands in for SSH transport. This is not a network SSH authentication test.
Build first, then run: python3 scripts/smoke-remote.py [--binary PATH]
"""
from __future__ import annotations

import argparse
import base64
from collections import deque
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import struct
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("remote_turn_helpers", Path(__file__).with_name("smoke-turns.py"))
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
require, SmokeFailure = HELPERS.require, HELPERS.SmokeFailure
LOCAL_KEY = "sk-only-local-remote-smoke-fixture"
REMOTE_KEY = "sk-only-remote-remote-smoke-fixture"


def wait_until(predicate: Any, timeout: float, message: str) -> None:
    deadline = time.monotonic() + timeout
    while not predicate():
        require(time.monotonic() < deadline, message)
        time.sleep(0.02)


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


class Provider:
    """Loopback-only Responses fixture. Key values never enter logs or failures."""
    def __init__(self, key: str, label: str, timeout: float):
        self.key, self.label, self.timeout = key, label, timeout
        self.posts: list[dict[str, Any]] = []
        self.models = 0
        self.errors: list[str] = []
        self.model_opened = threading.Event()
        self.model_release = threading.Event()
        self.model_release.set()
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            def log_message(self, *_: Any) -> None:
                pass

            def authenticate(self) -> None:
                require(self.headers.get("Authorization") == "Bearer " + owner.key, owner.label + " provider received the wrong fixture account")

            def do_GET(self) -> None:
                try:
                    self.authenticate()
                    require(self.path == "/v1/models", "unexpected model catalog endpoint")
                    owner.models += 1
                    owner.model_opened.set()
                    require(owner.model_release.wait(timeout=owner.timeout * 2), "model catalog fixture was not released")
                    self.reply("application/json", json.dumps({"data": [{"id": "local-mock-model", "object": "model", "owned_by": "fixture"}]}).encode())
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                    self.close_connection = True

            def do_POST(self) -> None:
                try:
                    self.authenticate()
                    require(self.path == "/v1/responses", "unexpected Responses endpoint")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size < 8_000_000, "invalid Responses request size")
                    body = json.loads(self.rfile.read(size))
                    require(body.get("model") == "local-mock-model", "unexpected fixture model")
                    owner.posts.append(body)
                    text = owner.label + " 自有账号完成"
                    message = {"type": "message", "id": "fixture-message-" + owner.label, "role": "assistant", "phase": "final", "content": [{"type": "output_text", "text": text}]}
                    events = [{"type": "response.output_text.delta", "delta": text}, {"type": "response.output_item.done", "item": message}, {"type": "response.completed", "response": {"status": "completed", "output": [message]}}]
                    data = "".join("data: " + json.dumps(event, ensure_ascii=False) + "\n\n" for event in events).encode()
                    self.reply("text/event-stream", data)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.errors.append(type(error).__name__)
                    self.close_connection = True

            def reply(self, content_type: str, data: bytes) -> None:
                self.send_response(200)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(data)
                self.wfile.flush()
                self.close_connection = True

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def close(self) -> None:
        self.model_release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def __enter__(self) -> Provider:
        return self

    def __exit__(self, *_: Any) -> None:
        self.close()


class AppServer(HELPERS.AppServer):
    def __init__(self, binary: Path, store: Path, workspace: Path, fixture_home: Path, fake_bin: Path, base: str, timeout: float):
        env = {"PATH": os.pathsep.join([str(fake_bin), str(Path(sys.executable).parent), "/usr/bin", "/bin"]), "HOME": str(fixture_home), "MCODEX_HOME": str(store), "MCODEX_NO_CODEX_IMPORT": "1", "OPENAI_BASE_URL": base,
               "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost", "LANG": "en_US.UTF-8", "OPENAI_LOCAL_LEAK_SENTINEL": "fixture-only", "CODEX_LOCAL_LEAK_SENTINEL": "fixture-only"}
        self.timeout, self.next_id, self.rpc_count = timeout, 0, 0
        self.replies: dict[int, dict[str, Any]] = {}
        self.notifications: list[dict[str, Any]] = []
        self.approval_requests: dict[int, dict[str, Any]] = {}
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr: deque[str] = deque(maxlen=40)
        self.process = subprocess.Popen([str(binary), "--model", "local-mock-model", "-C", str(workspace), "app-server"], cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.readers = [threading.Thread(target=self._stdout, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for reader in self.readers:
            reader.start()

    def send(self, method: str, params: dict[str, Any] | None = None) -> int:
        self.next_id += 1
        self.rpc_count += 1
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": self.next_id, "method": method, "params": params or {}}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        return self.next_id

    def response(self, request_id: int) -> dict[str, Any]:
        self.pump_until(lambda: request_id in self.replies, "pending remote RPC response")
        return self.replies.pop(request_id)

    def expect_error(self, method: str, params: dict[str, Any] | None = None, contains: str | None = None) -> str:
        response = self.response(self.send(method, params))
        require("error" in response, method + " unexpectedly succeeded")
        message = response["error"].get("message", "")
        if contains:
            require(contains.lower() in message.lower(), method + " returned an unexpected error category")
        return message

    def remote(self, connection: str, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        return self.rpc("remote/rpc", {"connectionId": connection, "method": method, "params": params or {}})

    def remote_event(self, connection: str, method: str, thread: str | None = None, turn: str | None = None) -> dict[str, Any]:
        def matching() -> list[dict[str, Any]]:
            return [note["params"]["params"] for note in self.notifications if note.get("method") == "remote/notification" and note.get("params", {}).get("connectionId") == connection and note["params"].get("method") == method
                    and (thread is None or note["params"]["params"].get("threadId", note["params"]["params"].get("thread", {}).get("id")) == thread)
                    and (turn is None or note["params"]["params"].get("turn", {}).get("id", note["params"]["params"].get("turnId")) == turn)]
        self.pump_until(lambda: bool(matching()), "forwarded " + method)
        return matching()[-1]

    def close(self) -> None:
        if self.process.stdin is not None and not self.process.stdin.closed:
            self.process.stdin.close()
        failed = False
        try:
            self.process.wait(timeout=self.timeout)
            failed = self.process.returncode != 0
        except subprocess.TimeoutExpired:
            failed = True
            self.process.kill()
            self.process.wait(timeout=2)
        finally:
            for reader in self.readers:
                reader.join(timeout=1)
            for stream in [self.process.stdout, self.process.stderr]:
                if stream is not None:
                    stream.close()
        require(not failed, "root app-server failed to exit cleanly after stdin EOF")


def write_key(store: Path, key: str) -> bytes:
    store.mkdir(parents=True, exist_ok=True)
    data = json.dumps({"auth_mode": "apikey", "OPENAI_API_KEY": key}).encode()
    (store / "auth.json").write_bytes(data)
    (store / "auth.json").chmod(0o600)
    (store / "config.toml").write_text('model = "local-mock-model"\n')
    return data


def fake_transport(root: Path, binary: Path, remote_store: Path, remote_home: Path, remote_cwd: Path, remote_base: str, identity: Path) -> tuple[Path, Path, Path]:
    fake_bin = root / "fake bin"
    fake_bin.mkdir()
    alias = root / "agent ' literal ; touch AGENT_QUOTE_BROKEN ; #"
    alias.symlink_to(binary)
    log = root / "ssh-argv.jsonl"
    wire = struct.pack(">I", 11) + b"ssh-ed25519" + struct.pack(">I", 32) + bytes(range(32))
    key_line = "[fixture.invalid]:2222 ssh-ed25519 " + base64.b64encode(wire).decode()
    config = {"alias": str(alias), "home": str(remote_home), "store": str(remote_store), "cwd": str(remote_cwd), "base": remote_base, "log": str(log), "identity": str(identity), "keyLine": key_line}
    prefix = "#!/usr/bin/env python3\nimport json,os,shlex,sys\nc=json.loads(" + repr(json.dumps(config)) + ")\n"
    ssh = prefix + '''a=sys.argv[1:]
assert not any(k.startswith(('OPENAI_','MCODEX_','CODEX_')) for k in os.environ), 'Local account environment leaked into SSH'
assert a[0]=='-T' and '--' in a, 'SSH must be noninteractive and use an option terminator'
end=a.index('--'); flags={}; options=[]; i=1
while i<end:
    assert a[i] in ('-o','-p','-l','-i'), 'Unexpected SSH argument'
    if a[i]=='-o':options.append(a[i+1])
    else:flags[a[i]]=a[i+1]
    i+=2
assert i==end and len(a[end+1:])==2 and a[end+1]=='fixture.invalid', 'Host escaped the SSH argument boundary'
assert flags=={'-p':'2222','-l':'fixture','-i':c['identity']}, 'SSH connection arguments changed'
required={'BatchMode=yes','PasswordAuthentication=no','KbdInteractiveAuthentication=no','NumberOfPasswordPrompts=0','StrictHostKeyChecking=yes','ConnectTimeout=10'}
assert required.issubset(set(options)), 'SSH authentication/host-key policy was weakened'
assert all('StrictHostKeyChecking=no' not in v and 'UserKnownHostsFile=/dev/null' not in v for v in options), 'SSH host-key bypass found'
assert any(v.startswith('KnownHostsCommand=/bin/cat ') and 'remote-known-hosts' in v and '%%h' in v for v in options), 'App trust file was not passed safely'
cmd=shlex.split(a[-1]);assert cmd==[c['alias'],'app-server','--listen','stdio://','--cd',c['cwd']], 'Remote command arguments changed or lost literal quoting'
with open(c['log'],'a') as f:f.write(json.dumps({'kind':'ssh','pid':os.getpid(),'parent':os.getppid(),'argv':a,'cleanAccountEnvironment':True})+'\\n')
e={'PATH':os.pathsep.join([os.path.dirname(sys.executable),'/usr/bin','/bin']),'HOME':c['home'],'MCODEX_HOME':c['store'],'MCODEX_NO_CODEX_IMPORT':'1','OPENAI_BASE_URL':c['base'],'NO_PROXY':'127.0.0.1,localhost','no_proxy':'127.0.0.1,localhost','LANG':'en_US.UTF-8'}
# exec keeps the fake transport PID equal to the actual Rust remote agent PID.
os.execve('/bin/sh',['sh','-c','exec '+a[-1]],e)
'''
    scanner = prefix + '''a=sys.argv[1:]
assert a==['-T','5','-p','2222','--','fixture.invalid'], 'Host-key probe arguments changed'
with open(c['log'],'a') as f:f.write(json.dumps({'kind':'keyscan','pid':os.getpid(),'argv':a})+'\\n')
print(c['keyLine'])
'''
    for name, source in [("ssh", ssh), ("ssh-keyscan", scanner)]:
        path = fake_bin / name
        path.write_text(source)
        path.chmod(0o700)
    return fake_bin, alias, log


def records(log: Path) -> list[dict[str, Any]]:
    return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []


def run(binary: Path, timeout: float) -> None:
    checks, rpc_count = 0, 0
    live: list[AppServer] = []
    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print("PASS " + label, flush=True)

    with tempfile.TemporaryDirectory(prefix="mcodex remote %h ' smoke-") as directory:
        root = Path(directory).resolve()
        local_store, remote_store = root / "local state", root / "remote state"
        local_home, remote_home = root / "local home", root / "remote home"
        local_cwd, remote_cwd = root / "local workspace", root / "remote cwd ' ; touch CWD_QUOTE_BROKEN ; #"
        for path in [local_home, remote_home, local_cwd, remote_cwd]:
            path.mkdir()
        local_auth = write_key(local_store, LOCAL_KEY)
        remote_auth = write_key(remote_store, REMOTE_KEY)
        (local_cwd / "fixture.txt").write_text("LOCAL workspace only\n")
        remote_text = "REMOTE workspace 中文\r\nline two\n"
        (remote_cwd / "fixture.txt").write_bytes(remote_text.encode())
        identity = root / "not a real SSH key"
        identity.write_text("fixture path only; fake SSH never reads this file")
        identity.chmod(0o600)
        with Provider(LOCAL_KEY, "LOCAL", timeout) as local_provider, Provider(REMOTE_KEY, "REMOTE", timeout) as remote_provider:
            fake_bin, alias, log = fake_transport(root, binary, remote_store, remote_home, remote_cwd, remote_provider.base_url, identity)
            def start() -> AppServer:
                app = AppServer(binary, local_store, local_cwd, local_home, fake_bin, local_provider.base_url, timeout)
                live.append(app)
                initialized = app.rpc("initialize")
                require(Path(initialized["codexHome"]) == local_store and Path(initialized["cwd"]) == local_cwd and initialized.get("codexImportDisabled") is True, "root isolation handshake failed")
                return app

            def stop(app: AppServer) -> None:
                nonlocal rpc_count
                if app in live:
                    live.remove(app)
                    rpc_count += app.rpc_count
                    app.close()

            try:
                app = start()
                created = app.rpc("remote/profile/create", {"name": "Isolated fake SSH", "host": "fixture.invalid", "user": "fixture", "port": 2222, "identityFile": str(identity), "remoteCwd": str(remote_cwd), "agentPath": str(alias)})["profile"]
                profile_id = created["id"]
                for change in [{"host": "-oProxyCommand=bad"}, {"host": "fixture.invalid;touch BAD_HOST"}, {"user": "fixture;bad"}, {"port": 0}, {"remoteCwd": "relative"}, {"identityFile": "relative"}, {"agentPath": "-bad"}]:
                    app.expect_error("remote/profile/update", {"id": profile_id, **change})
                app.rpc("remote/profile/update", {"id": profile_id, "name": "Persistent profile 中文"})
                persisted = app.rpc("remote/profile/list")["data"]
                require(len(persisted) == 1 and persisted[0]["name"] == "Persistent profile 中文" and persisted[0]["host"] == "fixture.invalid", "profile rejection changed saved values")
                require((local_store / "remote-profiles.json").stat().st_mode & 0o077 == 0, "profile store is not private")
                stop(app)
                app = start()
                require(app.rpc("remote/profile/list")["data"] == persisted, "profile did not survive root restart")
                passed("profile persistence, private storage, strict host/user/path/port validation")

                probe = app.rpc("remote/hostKey/probe", {"profileId": profile_id})
                require(probe.get("verified") is False and len(probe["keys"]) == 1 and len(probe["fingerprints"]) == 1, "fake key probe contract failed")
                app.expect_error("remote/hostKey/trust", {"profileId": profile_id, "keys": probe["keys"], "fingerprints": ["SHA256:wrong"]})
                app.rpc("remote/hostKey/trust", {"profileId": profile_id, "keys": probe["keys"], "fingerprints": probe["fingerprints"]})
                require((local_store / "remote-known-hosts").stat().st_mode & 0o077 == 0, "App known_hosts is not private")
                connected = app.rpc("remote/connect", {"id": profile_id})["connection"]
                cid = connected["id"]
                require(connected["state"] == "connected" and connected["initialize"]["userAgent"].startswith("mcodex/"), "fake transport did not initialize a real Rust backend")
                require(Path(connected["initialize"]["cwd"]) == remote_cwd and Path(connected["initialize"]["codexHome"]) == remote_store and connected["initialize"].get("codexImportDisabled") is True, "remote default cwd/store escaped the selected profile")
                states = [note["params"]["connection"]["state"] for note in app.notifications if note.get("method") == "remote/status" and note["params"]["connection"]["id"] == cid]
                require("connecting" in states and "connected" in states, "connection status notifications were not dispatched")
                first_pid = [row for row in records(log) if row["kind"] == "ssh"][-1]["pid"]
                require(first_pid != app.process.pid and alive(first_pid), "second Rust process is missing")
                app.expect_error("remote/profile/update", {"id": profile_id, "name": "Must disconnect first"}, "disconnect")
                app.expect_error("remote/profile/delete", {"id": profile_id}, "disconnect")
                passed("fake host-key probe/trust, strict SSH argv/environment, real child initialize and status notifications")

                thread = app.remote(cid, "thread/start", {"mode": "chat", "model": "local-mock-model"})["thread"]
                tid = thread["id"]
                require(Path(thread["cwd"]) == remote_cwd and thread["connectionId"] == cid, "thread did not use remote default cwd/scope")
                started_event = app.remote_event(cid, "thread/started", tid)
                require(started_event["thread"]["connectionId"] == cid, "remote thread notification lost its scope")
                app.remote(cid, "thread/name/set", {"threadId": tid, "name": "Remote persisted chat 中文"})
                require(app.remote(cid, "thread/read", {"threadId": tid})["thread"]["name"] == "Remote persisted chat 中文", "remote thread read/rename failed")
                executed = app.remote(cid, "command/exec", {"cwd": str(remote_cwd), "command": "printf '%s' 'REMOTE_EXEC_中文'; printf '\\n'; pwd", "sandboxPolicy": {"type": "dangerFullAccess"}})
                require(executed["exitCode"] == 0 and "REMOTE_EXEC_中文" in executed["output"] and str(remote_cwd) in executed["output"], "actual remote printf/pwd command did not execute")
                content = app.remote(cid, "fs/readFile", {"cwd": str(remote_cwd), "path": "fixture.txt"})
                require(content["content"] == remote_text, "remote fs/readFile returned local content or changed bytes")
                app.expect_error("remote/rpc", {"connectionId": cid, "method": "remote/status", "params": {}}, "recursive")
                passed("real remote thread start/read, scoped notification, command execution, workspace file content")

                original = b"Remote original CRLF\r\n" + "中文\n".encode() * 20000
                imported = app.remote(cid, "files/import", {"name": "remote original.txt", "mimeType": "text/plain", "dataBase64": base64.b64encode(original).decode()})["attachment"]
                aid = imported["id"]
                require(imported["connectionId"] == cid and imported["sha256"] == hashlib.sha256(original).hexdigest(), "remote file metadata hash/scope failed")
                downloaded = app.remote(cid, "files/read", {"id": aid, "includeOriginal": True})
                require(base64.b64decode(downloaded["content"]["dataBase64"], validate=True) == original, "remote original attachment bytes did not round-trip")
                require(aid in {file["id"] for file in app.remote(cid, "files/list")["data"]}, "remote attachment was not persisted")
                require(app.rpc("files/list")["data"] == [], "remote attachment leaked into local Library")
                require(tid not in {item["id"] for item in app.rpc("thread/list", {"limit": 100})["data"]}, "remote session leaked into local thread list")
                passed("remote file import/original-byte download and local/remote data isolation")

                turn = app.remote(cid, "turn/start", {"threadId": tid, "input": [{"type": "text", "text": "Use only the remote fixture account"}]})["turn"]["id"]
                completed = app.remote_event(cid, "turn/completed", tid, turn)
                require(completed["turn"]["status"] == "completed", "remote mock turn failed")
                history = app.remote(cid, "thread/read", {"threadId": tid, "includeTurns": True})["thread"]["turns"]
                require(any(item.get("text") == "REMOTE 自有账号完成" for turn_ in history for item in turn_["items"]), "remote Responses output was not saved")
                require(len(remote_provider.posts) == 1 and not local_provider.posts and not remote_provider.errors, "remote turn contacted the local account/provider")
                local_tid = app.rpc("thread/start", {"mode": "chat", "model": "local-mock-model"})["thread"]["id"]
                local_turn = app.rpc("turn/start", {"threadId": local_tid, "input": [{"type": "text", "text": "Use the local fixture account"}]})["turn"]["id"]
                require(app.event("turn/completed", local_tid, local_turn)["turn"]["status"] == "completed", "local mock turn failed")
                require(len(local_provider.posts) == 1 and len(remote_provider.posts) == 1 and not local_provider.errors, "local Responses request used the wrong account/provider")
                require((local_store / "auth.json").read_bytes() == local_auth and (remote_store / "auth.json").read_bytes() == remote_auth, "transport changed fixture credentials")
                passed("each real Rust agent uses its own private auth file and separate loopback Responses provider")

                remote_provider.model_opened.clear()
                remote_provider.model_release.clear()
                pending = app.send("remote/rpc", {"connectionId": cid, "method": "model/list", "params": {"refresh": True}})
                require(remote_provider.model_opened.wait(timeout), "pending remote catalog RPC did not reach its provider")
                disconnected = app.rpc("remote/disconnect", {"connectionId": cid})["connection"]
                require(disconnected["state"] == "disconnected", "explicit disconnect did not settle")
                failed = app.response(pending)
                require("error" in failed and "disconnect" in failed["error"]["message"].lower(), "disconnect did not fail the in-flight remote RPC")
                remote_provider.model_release.set()
                app.expect_error("remote/rpc", {"connectionId": cid, "method": "thread/read", "params": {"threadId": tid}}, "not connected")
                wait_until(lambda: not alive(first_pid), timeout, "disconnected fake SSH/Rust child was not reaped")
                require(app.rpc("remote/status", {"connectionId": cid})["connection"]["state"] == "disconnected", "late EOF changed an intentional disconnect into failure")
                require(any(note.get("method") == "remote/status" and note["params"]["connection"]["id"] == cid and note["params"]["connection"]["state"] == "disconnected" for note in app.notifications), "disconnected notification missing")
                passed("active disconnect fails pending RPCs, rejects stale requests, emits status, and reaps its child")

                second = app.rpc("remote/connect", {"id": profile_id})["connection"]["id"]
                require(second != cid, "reconnect reused an obsolete connection identity")
                resumed = app.remote(second, "thread/resume", {"threadId": tid})["thread"]
                require(resumed["name"] == "Remote persisted chat 中文" and resumed["connectionId"] == second and len(resumed["turns"]) == 1, "reconnect lost remote thread history or scope")
                require(base64.b64decode(app.remote(second, "files/read", {"id": aid, "includeOriginal": True})["content"]["dataBase64"]) == original, "reconnect lost original attachment bytes")
                second_pid = [row for row in records(log) if row["kind"] == "ssh"][-1]["pid"]
                stop(app)  # EOF with a connected remote child must clean it up.
                wait_until(lambda: not alive(second_pid), timeout, "root EOF left a live fake SSH/Rust child")
                passed("reconnect resumes persisted remote chat/files; root stdin EOF reaps an active connection")

                app = start()
                require(app.rpc("remote/profile/list")["data"] == persisted, "connected root restart changed persisted profiles")
                third = app.rpc("remote/connect", {"id": profile_id})["connection"]["id"]
                require(app.remote(third, "thread/resume", {"threadId": tid})["thread"]["name"] == "Remote persisted chat 中文", "second root restart lost remote history")
                app.rpc("remote/disconnect", {"connectionId": third})
                app.rpc("remote/profile/delete", {"id": profile_id})
                require(app.rpc("remote/profile/list")["data"] == [], "disconnected profile could not be deleted")
                stop(app)
                for row in records(log):
                    if row["kind"] == "ssh":
                        wait_until(lambda pid=row["pid"]: not alive(pid), timeout, "completed test left an SSH/Rust child")
                require(not list(local_cwd.glob("*QUOTE_BROKEN*")) and not (local_cwd / "BAD_HOST").exists(), "literal profile values executed as shell code")
                require(not local_provider.errors and not remote_provider.errors, "fixture provider detected protocol/auth errors")
                require(len(local_provider.posts) == len(remote_provider.posts) == 1, "disconnect/reconnect retried a model request")
                serialized = json.dumps(records(log))
                require(LOCAL_KEY not in serialized and REMOTE_KEY not in serialized, "transport diagnostics included fixture credentials")
                passed("second root restart, profile deletion, no automatic model retry, and zero live transport children")
            finally:
                remote_provider.model_release.set()
                for app in list(live):
                    try:
                        stop(app)
                    except Exception:
                        if app.process.poll() is None:
                            app.process.kill()
                            app.process.wait(timeout=2)
                # Only fixture-recorded PIDs with this unique path are eligible for cleanup.
                for row in records(log):
                    if row["kind"] == "ssh" and alive(row["pid"]):
                        command = subprocess.run(["ps", "-ww", "-p", str(row["pid"]), "-o", "command="], capture_output=True, text=True).stdout
                        if str(root) in command:
                            os.kill(row["pid"], signal.SIGKILL)
    print(f"PASS remote smoke: {checks} checks, {rpc_count} root JSONL RPCs, 3 real Rust child connections, 2 isolated Responses calls; fake SSH only; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=10)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        print("FAIL remote smoke: build the Rust executable first", file=sys.stderr)
        return 1
    try:
        run(binary, args.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        message = str(error).replace(LOCAL_KEY, "[redacted fixture key]").replace(REMOTE_KEY, "[redacted fixture key]")
        print("FAIL remote smoke: " + message, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
