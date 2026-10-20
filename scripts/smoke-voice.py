#!/usr/bin/env python3
"""Exercise real voice JSONL RPCs against disposable loopback HTTP mocks.

Build first: cargo build. Run: python3 scripts/smoke-voice.py.
No recording device, WebRTC engine, real credentials, or external service is used.
The HTTP fixtures only emulate signaling, hangup, dictation, and model context.
"""
from __future__ import annotations

import argparse
import base64
from collections import deque
from dataclasses import dataclass, field
from email import policy
from email.parser import BytesParser
import importlib.util
import io
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
import wave

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("mcodex_voice_turn_helpers", Path(__file__).with_name("smoke-turns.py"))
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
require, SmokeFailure = HELPERS.require, HELPERS.SmokeFailure
KEY = "local-test-only"
SECOND_KEY = "second-fixed-local-key"
INVALID_KEY = "invalid-fixed-local-key"
SDP = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=local-voice-fixture\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=rtpmap:111 opus/48000/2\r\na=sendrecv\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\na=sctp-port:5000\r\n"
DICTATION = "只返回输入框草稿，不自动写入聊天。"


class VoiceServer(HELPERS.AppServer):
    def __init__(self, binary: Path, store: Path, workspace: Path, home: Path, base_url: str, timeout: float):
        env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home), "CODEX_HOME": str(home / "codex"), "MCODEX_HOME": str(store), "MCODEX_NO_CODEX_IMPORT": "1", "OPENAI_BASE_URL": base_url, "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost", "LANG": "en_US.UTF-8", "TMPDIR": str(home)}
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

    def send(self, method: str, params: dict[str, Any]) -> int:
        self.next_id += 1
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": self.next_id, "method": method, "params": params}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        self.rpc_count += 1
        return self.next_id

    def receive(self, request_id: int, *, error: str | None = None) -> dict[str, Any]:
        self.pump_until(lambda: request_id in self.replies, f"RPC response {request_id}")
        response = self.replies.pop(request_id)
        public(response)
        if error is not None:
            require("error" in response and error.lower() in response["error"].get("message", "").lower(), f"expected a real RPC rejection containing {error!r}: {response}")
            return response["error"]
        require("error" not in response, f"voice RPC error: {response.get('error')}")
        return response["result"]

    def reject(self, method: str, params: dict[str, Any], contains: str = "") -> dict[str, Any]:
        return self.receive(self.send(method, params), error=contains)

    def history(self, thread_id: str) -> dict[str, Any]:
        return self.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]


@dataclass
class Gate:
    provider: str
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)


class Provider:
    """Extend the existing Responses handler with voice-only loopback routes."""
    def __init__(self, timeout: float):
        self.timeout = timeout
        self.responses = HELPERS.MockResponses(timeout)
        self.starts: queue.Queue[Gate] = queue.Queue()
        self.start_requests: queue.Queue[dict[str, Any]] = queue.Queue()
        self.transcriptions: queue.Queue[dict[str, Any]] = queue.Queue()
        self.hangup_requests: queue.Queue[str] = queue.Queue()
        self.gates: list[Gate] = []
        self.hangup_gates: dict[str, Gate] = {}
        self.all_starts: list[str] = []
        self.all_hangups: list[str] = []
        self.all_transcriptions: list[dict[str, Any]] = []
        self.active_key = KEY
        self.call_keys: dict[str, str] = {}
        self.validations: list[str] = []
        self.response_authorizations: list[str] = []
        self.base_url = self.responses.base_url + "/v1"
        owner = self
        original = self.responses.server.RequestHandlerClass

        class Handler(original):
            def do_GET(self) -> None:
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path == "/v1/models", "unexpected key-validation route")
                    auth = self.headers.get("Authorization", "")
                    require(auth.startswith("Bearer "), "key validation had no bearer credential")
                    key = auth[len("Bearer "):]
                    owner.validations.append(key)
                    require(key in {KEY, SECOND_KEY, INVALID_KEY}, "key validation used an unknown credential")
                    if key == INVALID_KEY:
                        self.reply(401, b'{"error":{"message":"invalid local fixture key"}}', "application/json")
                    else:
                        self.reply(200, b'{"data":[{"id":"local-mock-model"}]}', "application/json")
                except Exception as error:
                    owner.responses.errors.append(str(error))
                finally:
                    self.close_connection = True

            def do_POST(self) -> None:
                if self.path == "/v1/responses":
                    actual = self.headers.get("Authorization", "")
                    if actual != "Bearer " + owner.active_key:
                        owner.responses.errors.append("Responses request reused an unexpected credential")
                        self.close_connection = True
                        return
                    owner.response_authorizations.append(actual)
                    # The shared SSE fixture expects its original fixed key. Validate
                    # and retain the real wire header before adapting that fixture.
                    self.headers.replace_header("Authorization", "Bearer " + KEY)
                    self.path = "/responses"
                    return super().do_POST()
                self.connection.settimeout(owner.timeout)
                try:
                    is_hangup = self.path.startswith("/v1/realtime/calls/") and self.path.endswith("/hangup")
                    provider_id = self.path[len("/v1/realtime/calls/"):-len("/hangup")] if is_hangup else None
                    # Hangup must use the key captured by that call, even after a swap.
                    expected_key = owner.call_keys.get(provider_id) if is_hangup else owner.active_key
                    require(expected_key is not None and self.headers.get("Authorization") == "Bearer " + expected_key, "voice request reused an unexpected credential")
                    length = int(self.headers.get("Content-Length", "0"))
                    require(0 <= length <= 12 * 1024 * 1024, "unexpected voice request size")
                    raw = self.rfile.read(length)
                    if self.path == "/v1/realtime/calls":
                        fields = multipart(self.headers.get("Content-Type", ""), raw)
                        require(set(fields) == {"sdp", "session"}, "signaling multipart fields differ from sdp/session")
                        require(fields["sdp"]["data"].decode() == SDP and fields["sdp"]["mime"] == "application/sdp", "signaling SDP part was changed")
                        session = json.loads(fields["session"]["data"])
                        require(session["type"] == "realtime" and session["tools"] == [], "voice session exposed tools or used the wrong session type")
                        require(session["audio"]["input"]["transcription"]["model"] == "gpt-4o-mini-transcribe", "voice transcription model differs from backend capability")
                        detection = session["audio"]["input"]["turn_detection"]
                        require(detection["type"] == "server_vad" and detection["create_response"] is True and detection["interrupt_response"] is True, "voice VAD/barge-in settings were lost")
                        gate = owner.starts.get(timeout=owner.timeout)
                        owner.call_keys[gate.provider] = owner.active_key
                        owner.all_starts.append(gate.provider)
                        owner.start_requests.put({"session": session, "fields": fields, "provider": gate.provider})
                        gate.opened.set()
                        require(gate.release.wait(owner.timeout * 2), "delayed signaling response exceeded its fixture deadline")
                        body = SDP.encode()
                        self.reply(201, body, "application/sdp", {"Location": "/v1/realtime/calls/" + gate.provider})
                    elif self.path.startswith("/v1/realtime/calls/") and self.path.endswith("/hangup"):
                        provider = self.path[len("/v1/realtime/calls/"):-len("/hangup")]
                        require(provider in owner.all_starts, "hangup targeted an unknown provider call")
                        owner.all_hangups.append(provider)
                        owner.hangup_requests.put(provider)
                        gate = owner.hangup_gates.get(provider)
                        if gate is not None:
                            gate.opened.set()
                            require(gate.release.wait(owner.timeout * 2), "delayed hangup exceeded its fixture deadline")
                        self.reply(204, b"", "text/plain")
                    elif self.path == "/v1/audio/transcriptions":
                        fields = multipart(self.headers.get("Content-Type", ""), raw)
                        require(set(fields) == {"model", "response_format", "file"}, "dictation multipart fields are invalid")
                        require(fields["model"]["data"] == b"gpt-4o-mini-transcribe" and fields["response_format"]["data"] == b"json", "dictation used an unexpected model/format")
                        require(fields["file"]["data"] == audio_bytes() and fields["file"]["filename"] == "fixture.wav" and fields["file"]["mime"] == "audio/wav", "dictation changed the audio bytes/filename/MIME")
                        owner.all_transcriptions.append(fields)
                        owner.transcriptions.put(fields)
                        self.reply(200, json.dumps({"text": DICTATION}, ensure_ascii=False).encode(), "application/json")
                    else:
                        raise SmokeFailure("unexpected local voice route " + self.path)
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.responses.errors.append(str(error))
                finally:
                    self.close_connection = True

            def reply(self, status: int, body: bytes, mime: str, headers: dict[str, str] | None = None) -> None:
                self.send_response(status)
                self.send_header("Content-Type", mime)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                for key, value in (headers or {}).items():
                    self.send_header(key, value)
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()

        self.responses.server.RequestHandlerClass = Handler

    def plan(self, provider: str, delayed: bool = False) -> Gate:
        gate = Gate(provider)
        if not delayed:
            gate.release.set()
        self.gates.append(gate)
        self.starts.put(gate)
        return gate

    def block_hangup(self, provider: str) -> Gate:
        gate = Gate(provider)
        self.gates.append(gate)
        self.hangup_gates[provider] = gate
        return gate

    def wait(self, values: queue.Queue[Any], label: str) -> Any:
        try:
            return values.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure(f"{label} exceeded {self.timeout:g}s; mock errors={self.responses.errors}") from error

    def __enter__(self) -> Provider:
        return self

    def __exit__(self, *_: Any) -> None:
        for gate in self.gates:
            gate.release.set()
        self.responses.close()


def multipart(content_type: str, raw: bytes) -> dict[str, dict[str, Any]]:
    require(content_type.startswith("multipart/form-data;"), "voice request is not multipart/form-data")
    message = BytesParser(policy=policy.default).parsebytes(("Content-Type: " + content_type + "\r\nMIME-Version: 1.0\r\n\r\n").encode() + raw)
    require(message.is_multipart(), "multipart parser did not find any parts")
    result: dict[str, dict[str, Any]] = {}
    for part in message.iter_parts():
        name = part.get_param("name", header="content-disposition")
        require(name is not None and name not in result, "duplicate or nameless multipart part")
        result[name] = {"data": part.get_payload(decode=True), "mime": part.get_content_type(), "filename": part.get_filename()}
    return result


def public(value: Any) -> None:
    text = json.dumps(value, ensure_ascii=False)
    require(all(key not in text for key in [KEY, SECOND_KEY, INVALID_KEY]), "owned API key leaked in an RPC result or notification")


def model_public(value: Any) -> None:
    if isinstance(value, dict):
        require(not any(key.startswith("_mcodex_") for key in value), "private voice/presentation fields reached the Responses API")
        for child in value.values():
            model_public(child)
    elif isinstance(value, list):
        for child in value:
            model_public(child)


def audio_bytes() -> bytes:
    stream = io.BytesIO()
    with wave.open(stream, "wb") as audio:
        audio.setnchannels(1)
        audio.setsampwidth(2)
        audio.setframerate(8000)
        audio.writeframes(b"\0\0" * 80)
    return stream.getvalue()


def git(cwd: Path, *args: str) -> bytes:
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(cwd.parent), "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1", "GIT_AUTHOR_NAME": "Voice Smoke", "GIT_AUTHOR_EMAIL": "smoke@localhost", "GIT_COMMITTER_NAME": "Voice Smoke", "GIT_COMMITTER_EMAIL": "smoke@localhost", "GIT_TERMINAL_PROMPT": "0"}
    result = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgSign=false", "-C", str(cwd), *args], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=20)
    require(result.returncode == 0, f"temporary git fixture failed: {result.stderr.decode(errors='replace')}")
    return result.stdout


def history_text(thread: dict[str, Any]) -> list[str]:
    result = []
    for turn in thread.get("turns", []):
        for item in turn.get("items", []):
            if item.get("type") == "userMessage":
                result.append("".join(part.get("text", "") for part in item.get("content", [])))
            elif item.get("type") == "agentMessage":
                result.append(item.get("text", ""))
    return result


def records(store: Path, thread_id: str) -> list[dict[str, Any]]:
    for path in (store / "sessions").rglob("*.jsonl"):
        lines = [json.loads(line) for line in path.read_text().splitlines() if line]
        if lines and lines[0].get("meta", {}).get("id") == thread_id:
            return [line["item"] for line in lines if "item" in line]
    raise SmokeFailure("native voice session was not found in its isolated store")


def prepare_guard_fixtures(store: Path, workspace: Path) -> tuple[str, str, Path, bytes]:
    sessions = store / "sessions"
    sessions.mkdir()
    goal_id, imported_id = "voice_guard_goal", "voice_guard_imported"
    native = {"meta": {"id": goal_id, "cwd": str(workspace), "model": "local-mock-model", "created_ms": int(time.time() * 1000)}}
    (sessions / (goal_id + ".jsonl")).write_text(json.dumps(native) + "\n")
    timestamp = int(time.time())
    goal = {"threadId": goal_id, "objective": "Persisted active goal must block voice; not due during this smoke", "status": "active", "tokenBudget": None, "tokensUsed": 0, "timeUsedSeconds": 0, "updatedAt": timestamp, "createdAt": timestamp, "inFlight": False, "turnId": None, "suppressed": False, "reason": None, "evidence": [], "remaining": [], "nextPrompt": None, "activeSince": timestamp, "nextEligibleAt": timestamp + 3600, "blockerKey": None, "blockerCount": 0, "failureKey": None, "failureCount": 0, "turnsStarted": 0, "turnsCompleted": 0, "finishedTurnId": None}
    (store / "goals.json").write_text(json.dumps({"version": 1, "goals": {goal_id: goal}}))
    imported = sessions / "imported" / "official-rollout.jsonl"
    imported.parent.mkdir()
    source = (json.dumps({"type": "session_meta", "payload": {"id": imported_id, "cwd": str(workspace)}}) + "\n" + json.dumps({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "READ_ONLY_IMPORTED_CONTEXT"}]}}) + "\n").encode()
    imported.write_bytes(source)
    return goal_id, imported_id, imported, source


def voice_start(app: VoiceServer, provider: Provider, call_id: str, thread_id: str, provider_id: str) -> dict[str, Any]:
    provider.plan(provider_id)
    result = app.rpc("voice/start", {"id": call_id, "threadId": thread_id, "sdp": SDP, "voice": "marin"})
    public(result)
    require(result["id"] == call_id and result["threadId"] == thread_id and result["sdp"] == SDP and result["providerCallId"] == provider_id, "voice signaling returned an incorrect bound identity or SDP")
    request = provider.wait(provider.start_requests, "voice signaling request")
    require(request["provider"] == provider_id, "voice calls were reordered unexpectedly")
    require(request["session"]["audio"]["output"]["voice"] == "marin", "requested voice selection was lost")
    return request


def run(binary: Path, timeout: float) -> None:
    checks = rpc_count = 0
    with tempfile.TemporaryDirectory(prefix="mcodex-voice-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace, home = root / "mcodex", root / "workspace", root / "home"
        for path in [store, workspace, home]:
            path.mkdir()
        (store / "auth.json").write_text(json.dumps({"auth_mode": "apikey", "OPENAI_API_KEY": KEY}))
        (store / "auth.json").chmod(0o600)
        (store / "config.toml").write_text('model = "local-mock-model"\n')
        git(workspace, "init", "-b", "main")
        (workspace / "keep.txt").write_text("committed voice fixture\n")
        git(workspace, "add", "keep.txt")
        git(workspace, "commit", "-m", "local voice fixture")
        git(workspace, "branch", "spare")
        goal_id, imported_id, imported_path, imported_bytes = prepare_guard_fixtures(store, workspace)
        transcript = [{"id": "spoken_u1", "role": "user", "text": "真人语音第一条：记住项目约束。"}, {"id": "spoken_a1", "role": "assistant", "text": "语音助手第一条：已经记下。"}, {"id": "spoken_u2", "role": "user", "text": "真人语音末批：继续处理资料。"}, {"id": "spoken_a2", "role": "assistant", "text": "被打断的语音助手回应。", "interrupted": True}]
        with Provider(timeout) as provider:
            with VoiceServer(binary, store, workspace, home, provider.base_url, timeout) as app:
                initialized = app.rpc("initialize")
                require(initialized.get("codexImportDisabled") is True and Path(initialized["codexHome"]) == store, "binary did not confirm isolated voice state; rebuild first")
                capabilities = app.rpc("voice/capabilities")
                require(capabilities["available"] is True and capabilities["realtime"]["implemented"] is True and capabilities["realtime"]["requiresApiKey"] is True and capabilities["realtime"]["chatgptOAuthSupported"] is False and capabilities["dictation"]["implemented"] is True, "voice capability does not match the fixed API-key connection")
                first = app.rpc("thread/start", {"mode": "chat"})["thread"]["id"]
                other = app.rpc("thread/start", {"mode": "chat"})["thread"]["id"]
                app.reject("voice/start", {"id": "not_loaded", "threadId": "unknown_thread", "sdp": SDP}, "start or resume")
                app.reject("voice/start", {"id": "bad_sdp", "threadId": first, "sdp": "not SDP"}, "SDP")
                app.rpc("thread/resume", {"threadId": goal_id})
                require(app.rpc("thread/goal/get", {"threadId": goal_id})["goal"]["status"] == "active", "goal guard fixture is not active")
                app.reject("voice/start", {"id": "goal_rejected", "threadId": goal_id, "sdp": SDP}, "active goal")
                readonly = app.rpc("thread/resume", {"threadId": imported_id})["thread"]
                require(readonly["readOnly"] is True, "official-format fixture was not read-only")
                app.reject("voice/start", {"id": "imported_rejected", "threadId": imported_id, "sdp": SDP}, "imported")
                require(imported_path.read_bytes() == imported_bytes and provider.all_starts == [], "rejected voice calls contacted the provider or changed the imported source")
                app.rpc("thread/goal/set", {"threadId": goal_id, "status": "paused"})
                checks += 1
                print("PASS API-key capability, loaded-thread/SDP validation, active Goal and read-only import guards", flush=True)

                request = voice_start(app, provider, "call_main", first, "rtc_fixture")
                require("no local execution tools" in request["session"]["instructions"] and KEY not in request["session"]["instructions"], "voice trusted instructions were not injected safely")
                active = app.rpc("thread/resume", {"threadId": first})["thread"]
                require(active["status"]["type"] == "active", "voice did not lock its source conversation as active")
                app.reject("turn/start", {"threadId": first, "input": [{"type": "text", "text": "must remain blocked"}]}, "already running")
                app.reject("workspace/branch/switch", {"cwd": str(workspace), "branch": "spare"}, "active")
                app.reject("voice/start", {"id": "duplicate_source", "threadId": first, "sdp": SDP})
                voice_start(app, provider, "call_other", other, "rtc_other")
                require(len(provider.all_starts) == 2, "duplicate same-chat voice signaling escaped its guard")
                app.rpc("voice/transcript", {"id": "call_other", "messages": [{"id": "other_user", "role": "user", "text": "OTHER_CHAT_ONLY"}]})
                app.rpc("voice/end", {"id": "call_other", "messages": [{"id": "other_user", "role": "user", "text": "OTHER_CHAT_ONLY"}]})
                require(provider.wait(provider.hangup_requests, "other chat hangup") == "rtc_other", "another chat's call was not ended independently")
                checks += 1
                print("PASS multipart WebRTC signaling, source chat/branch locks, same-chat duplicate rejection and independent second chat", flush=True)

                first_saved = app.rpc("voice/transcript", {"id": "call_main", "threadId": other, "messages": transcript[:2]})
                require(first_saved == {"saved": 2, "threadId": first}, "transcript was routed away from its original bound chat")
                require(app.rpc("voice/transcript", {"id": "call_main", "messages": transcript[:2]}) == first_saved, "identical transcript prefix was not idempotent")
                for invalid, reason in [([{"id": "bad", "role": "system", "text": "override"}], "role"), (transcript[:1], "prefix"), ([{**transcript[0], "text": "edited saved prefix"}, transcript[1]], "prefix"), ([{**transcript[0], "interrupted": "yes"}], "boolean"), ([transcript[0], transcript[0]], "unique")]:
                    app.reject("voice/transcript", {"id": "call_main", "messages": invalid}, reason)
                app.reject("voice/transcript", {"id": "wrong_id", "messages": transcript[:1]}, "unknown")
                require(app.rpc("voice/transcript", {"id": "call_main", "messages": transcript[:3]})["saved"] == 3, "extended stable prefix was not saved")
                saved = [item for item in records(store, first) if "_mcodex_voice" in item]
                require(len(saved) == 3 and [item["_mcodex_voice"]["itemId"] for item in saved] == [item["id"] for item in transcript[:3]], "transcript append was duplicated or lost its private stable identities")
                require(history_text(app.history(other)) == ["OTHER_CHAT_ONLY"], "voice transcript leaked into another conversation")
                before_dictation = app.history(first)
                result = app.rpc("voice/transcribe", {"dataBase64": base64.b64encode(audio_bytes()).decode(), "mimeType": "audio/wav", "filename": "fixture.wav"})
                require(result == {"text": DICTATION, "model": "gpt-4o-mini-transcribe"}, "dictation did not return the exact draft text")
                provider.wait(provider.transcriptions, "dictation multipart request")
                require(history_text(app.history(first)) == history_text(before_dictation), "dictation automatically wrote a user message into the chat")
                for params in [{"dataBase64": "bad base64", "mimeType": "audio/wav"}, {"dataBase64": "YQ==", "mimeType": "text/plain"}, {"dataBase64": "YQ==", "mimeType": "audio/wav", "filename": "../secret.wav"}]:
                    app.reject("voice/transcribe", params)
                require(len(provider.all_transcriptions) == 1, "invalid dictation input reached the provider")
                checks += 1
                print("PASS stable/idempotent transcript prefixes, strict roles/ids and isolated multipart dictation drafts", flush=True)

                cleanup = provider.block_hangup("rtc_fixture")
                ending = app.send("voice/end", {"id": "call_main", "messages": transcript})
                require(cleanup.opened.wait(timeout), "voice/end never started provider cleanup")
                require(len([item for item in records(store, first) if "_mcodex_voice" in item]) == 3, "voice/end saved its final batch before provider cleanup")
                cleanup.release.set()
                require(app.receive(ending)["ended"] is True, "voice/end did not complete after cleanup")
                require(provider.wait(provider.hangup_requests, "main chat hangup") == "rtc_fixture", "main call cleanup targeted a wrong provider identity")
                require(app.rpc("voice/end", {"id": "call_main", "messages": transcript})["ended"] is True, "repeated voice/end was not idempotent")
                require(app.rpc("voice/end", {"id": "never_started", "messages": []})["ended"] is True, "unknown call with empty transcript was not idempotent")
                app.reject("voice/end", {"id": "../invalid", "messages": []}, "identifier")
                final_records = [item for item in records(store, first) if "_mcodex_voice" in item]
                require(len(final_records) == 4 and final_records[-1]["_mcodex_voice"]["interrupted"] is True, "final transcript batch or interrupted marker was lost")
                final_history = app.history(first)
                require(final_history["status"]["type"] == "idle" and history_text(final_history) == [item["text"] for item in transcript], "voice/end did not release the source thread or preserve ordered transcript text")
                require(all(item.get("source") == "voice" for turn in final_history["turns"] for item in turn["items"] if item.get("type") == "userMessage"), "history lost the voice source marker")
                app.rpc("workspace/branch/switch", {"cwd": str(workspace), "branch": "spare"})
                require(git(workspace, "symbolic-ref", "--short", "HEAD").strip() == b"spare", "voice cleanup did not release the checkout mutation guard")
                checks += 1
                print("PASS end cleans provider before final save, persists interruption/source flags, releases chat/checkout, and is idempotent", flush=True)

                delayed = provider.plan("rtc_late", delayed=True)
                pending = app.send("voice/start", {"id": "call_late", "threadId": first, "sdp": SDP})
                require(delayed.opened.wait(timeout), "pending voice start never reached the loopback provider")
                provider.wait(provider.start_requests, "pending signaling request")
                require(app.rpc("voice/end", {"id": "call_late", "messages": []})["ended"] is True, "end could not cancel pending signaling")
                app.receive(pending, error="cancelled")
                delayed.release.set()
                require(provider.wait(provider.hangup_requests, "late provider cleanup") == "rtc_late", "late successful provider call escaped cleanup after user cancellation")
                require(app.history(first)["status"]["type"] == "idle", "late signaling reactivated the cancelled thread")
                require(history_text(app.history(first)) == [item["text"] for item in transcript], "cancelled pending voice start inserted false transcript messages")
                checks += 1
                print("PASS end-before-signaling-answer cancels promptly and hangs up the late provider call", flush=True)
                public(app.notifications)
                rpc_count += app.rpc_count

            with VoiceServer(binary, store, workspace, home, provider.base_url, timeout) as app:
                app.rpc("initialize")
                restored = app.rpc("thread/resume", {"threadId": first})["thread"]
                require(history_text(restored) == [item["text"] for item in transcript], "restart lost/duplicated the real voice transcript")
                app.rpc("thread/resume", {"threadId": other})
                require(history_text(app.history(other)) == ["OTHER_CHAT_ONLY"], "restart mixed separate voice conversations")
                scenario = provider.responses.plan("success", "你好，已使用保存的语音上下文 🙂")
                turn_id = app.turn(first, [{"type": "text", "text": "Continue from the saved voice conversation."}])
                body = provider.responses.request()
                model_public(body)
                context = [part["text"] for item in body["input"] if item.get("role") in {"user", "assistant"} for part in item.get("content", []) if "text" in part]
                for item in transcript:
                    require(context.count(item["text"]) == 1, "restarted model context lost or duplicated a voice message")
                require("OTHER_CHAT_ONLY" not in context, "another conversation's voice text entered model context")
                app.assert_chinese_completion(first, turn_id, scenario.text)
                checks += 1
                print("PASS restart and next model turn retain transcript context exactly once without private voice fields", flush=True)

                voice_start(app, provider, "call_key_swap", first, "rtc_key_swap")
                previous_hangups = len(provider.all_hangups)
                require(app.rpc("account/read")["account"]["type"] == "apiKey", "ordinary metadata read lost the connected key")
                require(app.rpc("voice/capabilities")["available"] is True and app.history(first)["status"]["type"] == "active", "ordinary metadata/capability read stopped voice")
                app.reject("account/login/start", {"type": "apiKey", "apiKey": INVALID_KEY}, "401")
                require(len(provider.all_hangups) == previous_hangups and provider.hangup_requests.empty(), "failed key validation stopped the existing voice call")
                require(app.history(first)["status"]["type"] == "active" and json.loads((store / "auth.json").read_text())["OPENAI_API_KEY"] == KEY, "failed key validation changed credentials or released the active voice lock")
                switched = app.rpc("account/login/start", {"type": "apiKey", "apiKey": SECOND_KEY})
                public(switched)
                require(switched["account"]["type"] == "apiKey" and provider.validations[-2:] == [INVALID_KEY, SECOND_KEY], "new key did not pass a real local models validation")
                require(provider.wait(provider.hangup_requests, "key swap voice cleanup") == "rtc_key_swap", "successful key change did not hang up the old call")
                require(app.history(first)["status"]["type"] == "idle", "successful key change left the old voice chat locked")
                require(json.loads((store / "auth.json").read_text())["OPENAI_API_KEY"] == SECOND_KEY and (store / "auth.json").stat().st_mode & 0o077 == 0, "successful key validation did not save the new private credential")
                provider.active_key = SECOND_KEY
                switched_reply = provider.responses.plan("success", "你好，换 key 后已释放旧语音会话 🙂")
                switched_turn = app.turn(first, [{"type": "text", "text": "Use the newly validated local test key."}])
                model_public(provider.responses.request())
                require(provider.response_authorizations[-1] == "Bearer " + SECOND_KEY, "new model turn reused the old loaded-client Authorization")
                app.assert_chinese_completion(first, switched_turn, switched_reply.text)
                voice_start(app, provider, "call_new_key", first, "rtc_new_key")
                require(provider.call_keys["rtc_new_key"] == SECOND_KEY, "new voice signaling reused old Authorization")
                app.rpc("voice/end", {"id": "call_new_key", "messages": []})
                require(provider.wait(provider.hangup_requests, "new-key hangup") == "rtc_new_key", "new-key call did not clean up independently")
                checks += 1
                print("PASS failed key validation/read preserves active voice; successful key change hangs up old call and new turn/voice use new Authorization", flush=True)

                voice_start(app, provider, "call_logout", first, "rtc_logout")
                require(app.rpc("account/logout")["account"] is None, "logout kept an owned account connected")
                require(provider.wait(provider.hangup_requests, "logout voice cleanup") == "rtc_logout", "logout did not hang up the real provider call")
                require(app.rpc("voice/capabilities")["available"] is False, "signed-out voice capability remained available")
                before = len(provider.all_starts)
                app.reject("voice/start", {"id": "after_logout", "threadId": first, "sdp": SDP}, "API key")
                app.reject("voice/transcribe", {"dataBase64": base64.b64encode(audio_bytes()).decode(), "mimeType": "audio/wav", "filename": "fixture.wav"}, "API key")
                require(len(provider.all_starts) == before and len(provider.all_transcriptions) == 1, "signed-out voice reused an old cached key")
                require(app.history(first)["status"]["type"] == "idle", "logout left the source chat locked by voice")
                public(app.notifications)
                checks += 1
                print("PASS logout hangs up voice, releases chat, and prevents cached-key signaling/dictation reuse", flush=True)
                rpc_count += app.rpc_count

            # OAuth availability is checked with fixed local tokens only; no exchange occurs.
            (store / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "tokens": {"access_token": "local-oauth-access-unused", "refresh_token": "local-oauth-refresh-unused", "account_id": "local-oauth-account"}}))
            (store / "auth.json").chmod(0o600)
            with VoiceServer(binary, store, workspace, home, provider.base_url, timeout) as app:
                app.rpc("initialize")
                capability = app.rpc("voice/capabilities")
                require(capability["available"] is False and capability["realtime"]["chatgptOAuthSupported"] is False and capability["realtime"]["requiresApiKey"] is True and "API key" in capability.get("reason", ""), "ChatGPT sign-in was incorrectly advertised as an API voice connection")
                app.rpc("thread/resume", {"threadId": first})
                before = len(provider.all_starts)
                app.reject("voice/start", {"id": "oauth_rejected", "threadId": first, "sdp": SDP}, "OAuth")
                require(len(provider.all_starts) == before, "OAuth voice attempted an external/provider API call")
                rpc_count += app.rpc_count
                checks += 1
                print("PASS OAuth capability remains explicitly unavailable without token exchange or provider requests", flush=True)
            require(provider.responses.errors == [], f"loopback provider errors: {provider.responses.errors}")
            require(provider.starts.empty() and provider.responses.plans.empty() and provider.responses.requests.empty(), "voice/model scenarios did not match executed requests")
            require(sorted(provider.all_starts) == sorted(provider.all_hangups), "a provider call was leaked or cleaned more than once")
            require(provider.response_authorizations == ["Bearer " + KEY, "Bearer " + SECOND_KEY], "model Authorization history did not follow validated key changes")
            calls, transcriptions, responses = len(provider.all_starts), len(provider.all_transcriptions), len(provider.response_authorizations)
    print(f"PASS voice smoke: {checks} groups, {rpc_count} JSONL RPCs, {calls} local signaling calls, {calls} hangups, {transcriptions} dictation mock, {responses} Responses mocks; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "debug" / "mcodex")
    parser.add_argument("--timeout", type=float, default=10, help="seconds per RPC/event/provider wait (default: 10)")
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        print(f"FAIL executable not found: {binary}; run cargo build first", file=sys.stderr)
        return 1
    try:
        run(binary, args.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, StopIteration, subprocess.TimeoutExpired) as error:
        print(f"FAIL voice smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
