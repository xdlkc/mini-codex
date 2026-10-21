#!/usr/bin/env python3
"""Exercise Goals and Scheduled tasks through the real Rust app-server.

Only a loopback mock Responses API is used. Product files, credentials, Git
repositories, worktrees, and run records live in one temporary directory.
Build first, then run: python3 scripts/smoke-automations.py
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
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


sys.dont_write_bytecode = True
HELPER_PATH = Path(__file__).with_name("smoke-turns.py")
SPEC = importlib.util.spec_from_file_location("mcodex_turn_smoke_helpers", HELPER_PATH)
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
AppServer, require, SmokeFailure = HELPERS.AppServer, HELPERS.require, HELPERS.SmokeFailure


@dataclass
class Response:
    kind: str
    tokens: int = 1
    name: str | None = None
    arguments: dict[str, Any] = field(default_factory=dict)
    marker: str = ""
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)


class MockResponses:
    """Return real SSE function-call frames and real provider usage fields."""

    def __init__(self, timeout: float):
        self.timeout = timeout
        self.plans: queue.Queue[Response] = queue.Queue()
        self.requests: queue.Queue[dict[str, Any]] = queue.Queue()
        self.history: list[dict[str, Any]] = []
        self.responses: list[Response] = []
        self.errors: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_: Any) -> None:
                pass

            def do_POST(self) -> None:
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path == "/responses", f"unexpected mock endpoint: {self.path}")
                    require(self.headers.get("Authorization") == "Bearer local-test-only", "mock received a non-test credential")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size < 8_000_000, "invalid mock request length")
                    body = json.loads(self.rfile.read(size))
                    require(body.get("model") == "local-mock-model", "unexpected model")
                    require(not contains_private_marker(body.get("input", [])), "private _mcodex_internal metadata leaked into a real Responses request")
                    response = owner.plans.get(timeout=owner.timeout)
                    if response.marker:
                        require(response.marker in body.get("instructions", "") or response.marker in last_user_text(body), f"wrong turn consumed mock plan {response.marker!r}")
                    if response.name:
                        require(response.name in {tool.get("name") for tool in body.get("tools", [])}, f"{response.name} was not available to this turn")
                    owner.history.append(body)
                    owner.requests.put(body)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if response.kind == "failed":
                        self.event({"type": "response.failed", "response": {"status": "failed", "error": {"message": "local automation fixture failure"}}})
                    else:
                        if response.kind == "slow":
                            self.event({"type": "response.output_text.delta", "delta": "本地测试处理中…"})
                            response.opened.set()
                            require(response.release.wait(owner.timeout), "slow fixture was not released")
                        call = {"type": "function_call", "id": f"fixture-item-{id(response)}", "call_id": f"fixture-call-{id(response)}", "name": response.name, "arguments": json.dumps(response.arguments, ensure_ascii=False)}
                        self.event({"type": "response.output_item.done", "item": call})
                        self.event({"type": "response.completed", "response": {"status": "completed", "output": [call], "usage": {"input_tokens": response.tokens, "output_tokens": 0, "total_tokens": response.tokens}}})
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Cancelling a real Rust request intentionally drops the stream.
                except Exception as error:
                    owner.errors.append(str(error))
                finally:
                    self.close_connection = True

            def event(self, event: dict[str, Any]) -> None:
                frame = ("data: " + json.dumps(event, ensure_ascii=False) + "\r\n\r\n").encode()
                self.wfile.write(f"{len(frame):x}\r\n".encode() + frame + b"\r\n")
                self.wfile.flush()

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def plan(self, name: str, arguments: dict[str, Any], tokens: int, marker: str = "", slow: bool = False) -> Response:
        response = Response("slow" if slow else "call", tokens, name, arguments, marker)
        self.responses.append(response)
        self.plans.put(response)
        return response

    def checkpoint(self, status: str, tokens: int, marker: str, **extra: Any) -> Response:
        arguments = {"status": status, "summary": f"Verified {marker}", "evidence": [f"Checked {marker}"], "remaining": [] if status == "complete" else ["Complete the final verification"]}
        arguments.update(extra)
        return self.plan("goal_checkpoint", arguments, tokens, marker)

    def report(self, summary: str, has_update: bool, tokens: int = 2) -> Response:
        return self.plan("scheduled_task_report", {"summary": summary, "hasUpdate": has_update}, tokens)

    def shell(self, artifact: str, tokens: int, marker: str = "", approval: bool = False) -> Response:
        require(artifact.replace("-", "").replace(".", "").isalnum(), "artifact must be a fixed safe fixture filename")
        arguments={"command": ["/bin/sh", "-c", f"printf fixture-work > {artifact}"]}
        if approval:arguments.update(sandbox_permissions='require_escalated',justification='Exercise the explicit approval boundary in an isolated fixture')
        return self.plan("shell", arguments, tokens, marker)

    def fail(self) -> None:
        response = Response("failed")
        self.responses.append(response)
        self.plans.put(response)

    def request(self) -> dict[str, Any]:
        try:
            return self.requests.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure(f"No Responses request within {self.timeout}s; mock errors={self.errors}") from error

    def __enter__(self) -> MockResponses:
        return self

    def __exit__(self, *_: Any) -> None:
        for response in self.responses:
            response.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def last_user_text(body: dict[str, Any]) -> str:
    messages = [item for item in body.get("input", []) if item.get("role") == "user"]
    return "".join(part.get("text", "") for part in messages[-1].get("content", [])) if messages else ""


def contains_private_marker(value: Any) -> bool:
    if isinstance(value, dict):
        return "_mcodex_internal" in value or any(contains_private_marker(child) for child in value.values())
    if isinstance(value, list):
        return any(contains_private_marker(child) for child in value)
    return False


def assert_user_message_flags(app: AppServer, thread_id: str, expected: list[bool], live: bool = True) -> None:
    items = [item for turn in history(app, thread_id) for item in turn.get("items", []) if item.get("type") == "userMessage"]
    require(len(items) == len(expected), f"unexpected persisted user message count for internal-marker check: {items}")
    require(all(item.get("internal", False) is flag for item, flag in zip(items, expected)), f"persisted user message internal flags do not match {expected}: {items}")
    if live:
        for method in ["item/started", "item/completed"]:
            emitted = [event["params"]["item"] for event in app.notifications if event.get("method") == method and event.get("params", {}).get("threadId") == thread_id and event["params"].get("item", {}).get("type") == "userMessage"]
            require(len(emitted) == len(expected) and all(item.get("internal", False) is flag for item, flag in zip(emitted, expected)), f"{method} user message internal flags do not match {expected}: {emitted}")


def poll(app: AppServer, read: Callable[[], Any], accept: Callable[[Any], bool], label: str, approve: bool = False) -> Any:
    deadline = time.monotonic() + app.timeout
    while time.monotonic() < deadline:
        value = read()
        if approve:
            for request_id in list(app.approval_requests):
                app.respond_approval(request_id, "accept")
                app.approval_requests.pop(request_id)
        if accept(value):
            return value
        time.sleep(0.04)
    raise SmokeFailure(f"{label} exceeded {app.timeout}s; last={value!r}; stderr={list(app.stderr)!r}")


def goal(app: AppServer, thread_id: str) -> dict[str, Any] | None:
    return app.rpc("thread/goal/get", {"threadId": thread_id})["goal"]


def wait_goal(app: AppServer, thread_id: str, predicate: Callable[[dict[str, Any]], bool], label: str, approve: bool = False) -> dict[str, Any]:
    return poll(app, lambda: goal(app, thread_id), lambda g: g is not None and predicate(g), label, approve)


def start_goal(app: AppServer, objective: str, budget: int | None = None) -> str:
    thread_id = app.rpc("thread/start", {"mode": "codex"})["thread"]["id"]
    options: dict[str, Any] = {"threadId": thread_id, "objective": objective, "status": "active"}
    if budget is not None:
        options["tokenBudget"] = budget
    app.rpc("thread/goal/set", options)
    return thread_id


def history(app: AppServer, thread_id: str) -> list[dict[str, Any]]:
    return app.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]["turns"]


def no_internal_tools_in_timeline(app: AppServer, thread_id: str) -> None:
    turns = history(app, thread_id)
    for turn in turns:
        for item in turn.get("items", []):
            require(not any(item.get(key) in ["goal_checkpoint", "scheduled_task_report"] for key in ["tool", "toolName", "name"]), "internal bookkeeping tool leaked into persisted user timeline")
    for event in app.notifications:
        params = event.get("params", {})
        if params.get("threadId") == thread_id and event.get("method") in ["item/started", "item/completed"]:
            item = params.get("item", {})
            require(not any(item.get(key) in ["goal_checkpoint", "scheduled_task_report"] for key in ["tool", "toolName", "name"]), "internal bookkeeping tool leaked into live tool timeline")


def expect_error(app: AppServer, method: str, params: dict[str, Any], label: str) -> None:
    try:
        app.rpc(method, params)
    except SmokeFailure as error:
        require("RPC error:" in str(error), f"{label} failed through a transport error")
    else:
        raise SmokeFailure(f"{label} unexpectedly succeeded")


def schedule_after(seconds: int = 86_400, count: int = 1) -> dict[str, Any]:
    date = datetime.now(timezone.utc) + timedelta(seconds=seconds)
    return {"dtstart": date.strftime("DTSTART:%Y%m%dT%H%M%SZ"), "rrule": f"FREQ=MINUTELY;INTERVAL=1;COUNT={count}", "timezone": "UTC"}


def create_task(app: AppServer, workspace: Path, name: str, **overrides: Any) -> dict[str, Any]:
    params = {"name": name, "prompt": f"Check {name} and report the real result", "cwd": str(workspace), "model": "local-mock-model", "mode": "codex", "kind": "cron", "state": "paused", "schedule": schedule_after()}
    params.update(overrides)
    return app.rpc("scheduler/create", params)["task"]


def wait_run(app: AppServer, task_id: str, run_id: str | None = None, approve: bool = False) -> dict[str, Any]:
    rows = poll(app, lambda: app.rpc("scheduler/runs", {"taskId": task_id})["data"], lambda rows: any((run_id is None or row["id"] == run_id) and row["status"] != "running" for row in rows), "scheduled run completion", approve)
    return next(row for row in rows if (run_id is None or row["id"] == run_id) and row["status"] != "running")


def run_notice(app: AppServer, run_id: str) -> dict[str, Any]:
    def matching() -> list[dict[str, Any]]:
        return [n["params"] for n in app.notifications if n.get("method") == "scheduler/run/updated" and n.get("params", {}).get("run", {}).get("id") == run_id and "shouldNotify" in n["params"]]
    poll(app, lambda: (app.rpc("scheduler/runs"), matching())[1], bool, "scheduled notification decision")
    return matching()[-1]


def initialize(app: AppServer, store: Path) -> None:
    state = app.rpc("initialize")
    require(state.get("codexImportDisabled") is True and Path(state["codexHome"]) == store, "binary did not confirm isolated test data")


def assert_quiet(app: AppServer, mock: MockResponses, label: str) -> None:
    count = len(mock.history)
    deadline = time.monotonic() + 0.65
    while time.monotonic() < deadline:
        app.rpc("thread/goal/list")
        time.sleep(0.04)
    require(len(mock.history) == count, f"{label} unexpectedly called the model")


def run(binary: Path, timeout: float) -> None:
    checks = 0
    rpc_total = 0

    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print(f"PASS {label}", flush=True)

    with tempfile.TemporaryDirectory(prefix="mcodex-automation-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace = root / "home", root / "workspace"
        store.mkdir()
        workspace.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        with MockResponses(timeout) as mock:
            app = AppServer(binary, store, workspace, mock.base_url, timeout)
            try:
                initialize(app, store)
                mock.shell("goal-artifact.txt", 7, "GOAL_CHAIN", approval=True)
                mock.checkpoint("active", 11, "GOAL_CHAIN", nextPrompt="GOAL_CHAIN final verification")
                mock.checkpoint("complete", 17, "GOAL_CHAIN")
                chain = start_goal(app, "GOAL_CHAIN write an artifact and verify the entire objective")
                complete = wait_goal(app, chain, lambda g: g["status"] == "complete" and not g["inFlight"], "two-turn goal completion", approve=True)
                bodies = [mock.request() for _ in range(3)]
                require(complete["tokensUsed"] == 35 and complete["turnsStarted"] == 2 and complete["turnsCompleted"] == 2, f"incorrect real goal accounting: {complete}")
                require(complete["remaining"] == [] and complete["evidence"], "goal finished without verified completion")
                require((workspace / "goal-artifact.txt").read_text() == "fixture-work", "approved shell work did not produce the artifact")
                require(any(i.get("type") == "function_call_output" for i in bodies[1]["input"]), "shell result was not returned to the model")
                require("GOAL_CHAIN final verification" in last_user_text(bodies[2]), "automatic continuation ignored nextPrompt")
                require(len(history(app, chain)) == 2, "automatic goal continuation created another chat or lost a turn")
                assert_user_message_flags(app, chain, [False, True])
                no_internal_tools_in_timeline(app, chain)
                passed("Goal shell approval, same-chat automatic continuation, checkpoints, real usage, and hidden bookkeeping tools")

                mock.shell("queue-artifact.txt", 2, "GOAL_QUEUE_PRIORITY", approval=True)
                mock.checkpoint("active", 3, "GOAL_QUEUE_PRIORITY", nextPrompt="AUTOMATIC_PROMPT_MUST_NOT_WIN")
                mock.checkpoint("complete", 5, "GOAL_QUEUE_PRIORITY")
                queued_goal = start_goal(app, "GOAL_QUEUE_PRIORITY finish work after following the user's steering")
                mock.request()
                request = app.approval(queued_goal)
                original = goal(app, queued_goal)
                require(original is not None and original["inFlight"], "goal was not actually waiting for approval")
                expect_error(app, "thread/goal/set", {"threadId": queued_goal, "objective": "汉" * 4001, "edit": True}, "oversized in-flight goal edit")
                expect_error(app, "thread/goal/set", {"threadId": queued_goal, "objective": "UNCONFIRMED_REPLACEMENT"}, "unconfirmed in-flight replacement")
                unchanged = goal(app, queued_goal)
                fields = ["objective", "status", "tokensUsed", "turnsStarted", "turnId", "inFlight", "tokenBudget"]
                require(unchanged is not None and all(unchanged[field] == original[field] for field in fields), "invalid edits paused, replaced, or reset the running goal")
                require(any(row["id"] == request["id"] for row in app.rpc("approval/list", {"threadId": queued_goal})["data"]), "invalid goal edit cancelled an existing approval")
                followup = "MANUAL_FOLLOWUP_PRIORITY verify my queued instruction first"
                queued = app.rpc("turn/queue", {"threadId": queued_goal, "input": [{"type": "text", "text": followup}]})
                require(queued.get("queued") is True, "human follow-up was not queued")
                app.respond_approval(request["id"], "accept")
                app.approval_requests.pop(request["id"], None)
                completed_queue = wait_goal(app, queued_goal, lambda g: g["status"] == "complete" and not g["inFlight"], "queued follow-up goal completion")
                mock.request()
                next_body = mock.request()
                require(last_user_text(next_body) == followup, "automatic goal nextPrompt ran before the queued user follow-up")
                require(completed_queue["tokensUsed"] == 10 and completed_queue["turnsStarted"] == 2 and completed_queue["turnsCompleted"] == 2, "queued human turn lost goal usage or turn accounting")
                queue_events = [n["params"] for n in app.notifications if n.get("method") == "thread/queue/updated" and n.get("params", {}).get("threadId") == queued_goal]
                require(queue_events and queue_events[-1]["count"] == 0, "human message queue was not drained")
                assert_user_message_flags(app, queued_goal, [False, False])
                assert_quiet(app, mock, "completed queued goal")
                passed("human queue beats automatic continuation; invalid edits preserve the running goal and approval")

                mock.checkpoint("complete", 25, "GOAL_BUDGET")
                budget = start_goal(app, "GOAL_BUDGET must not complete after the limit", 20)
                limited = wait_goal(app, budget, lambda g: g["status"] == "budgetLimited" and not g["inFlight"], "goal budget limit")
                mock.request()
                require(limited["tokensUsed"] == 25 and limited["status"] != "complete", "budget limit became completion or usage was estimated")
                assert_quiet(app, mock, "budget-limited goal")
                passed("real token budget stops work without false completion")

                mock.checkpoint("active", 3, "GOAL_NO_WORK")
                suppressed = start_goal(app, "GOAL_NO_WORK inspect the target before finishing")
                paused = wait_goal(app, suppressed, lambda g: g["suppressed"] and not g["inFlight"], "no-work continuation suppression")
                mock.request()
                require(paused["status"] == "active", "no-work suppression incorrectly completed or paused the objective")
                assert_quiet(app, mock, "suppressed goal")
                mock.checkpoint("complete", 4, "GOAL_NO_WORK")
                app.rpc("thread/goal/set", {"threadId": suppressed, "status": "active"})
                resumed = wait_goal(app, suppressed, lambda g: g["status"] == "complete" and not g["inFlight"], "explicit goal resume")
                mock.request()
                require(resumed["tokensUsed"] == 7 and resumed["turnsCompleted"] == 2, "resume reset usage or failed to continue")
                passed("no-work suppression and explicit Resume")

                slow = mock.checkpoint("complete", 999, "GOAL_PAUSE_SLOW")
                slow.kind = "slow"
                slow_id = start_goal(app, "GOAL_PAUSE_SLOW wait for the stream")
                mock.request()
                require(slow.opened.wait(timeout), "slow goal stream did not open")
                app.rpc("thread/goal/set", {"threadId": slow_id, "status": "paused"})
                slow.release.set()
                paused = goal(app, slow_id)
                require(paused is not None and paused["status"] == "paused" and not paused["inFlight"] and paused["tokensUsed"] == 0, "pause accepted late SSE usage or left an active goal")

                slow = mock.checkpoint("complete", 999, "GOAL_REPLACE_OLD")
                slow.kind = "slow"
                replacement = start_goal(app, "GOAL_REPLACE_OLD old objective")
                mock.request()
                require(slow.opened.wait(timeout), "replacement stream did not open")
                app.rpc("thread/goal/set", {"threadId": replacement, "objective": "GOAL_REPLACE_NEW replacement", "replace": True, "status": "paused"})
                slow.release.set()
                replaced = goal(app, replacement)
                require(replaced is not None and replaced["objective"].startswith("GOAL_REPLACE_NEW") and replaced["tokensUsed"] == 0 and replaced["turnsStarted"] == 0, "old in-flight work was charged to the replacement goal")
                passed("pause and replacement cancel slow streams without charging late usage")

                mock.shell("never-paused.txt", 5, "GOAL_EDIT_APPROVAL", approval=True)
                edited = start_goal(app, "GOAL_EDIT_APPROVAL old instructions")
                mock.request()
                app.approval(edited)
                app.rpc("thread/goal/set", {"threadId": edited, "status": "paused"})
                changed = app.rpc("thread/goal/set", {"threadId": edited, "objective": "GOAL_EDIT_APPROVAL revised instructions", "edit": True})["goal"]
                require(changed["tokensUsed"] == 5 and changed["status"] == "paused", "editing reset real usage or resumed paused work")
                require(not (workspace / "never-paused.txt").exists(), "paused approval executed its shell")

                mock.shell("never-cleared.txt", 6, "GOAL_CLEAR_APPROVAL", approval=True)
                cleared = start_goal(app, "GOAL_CLEAR_APPROVAL clear while waiting")
                mock.request()
                app.approval(cleared)
                app.rpc("thread/goal/clear", {"threadId": cleared})
                require(goal(app, cleared) is None and not (workspace / "never-cleared.txt").exists(), "cleared goal persisted or executed its pending shell")
                require(app.rpc("approval/list", {"threadId": cleared}).get("data", []) == [], "cleared goal kept a pending approval")
                passed("pause/edit preserve usage; Clear cancels waiting approvals")

                mock.report("Manual cron completed", True)
                cron = create_task(app, workspace, "PAUSED_CRON")
                claim = app.rpc("scheduler/run", {"id": cron["id"]})
                cron_run = wait_run(app, cron["id"], claim["run"]["id"])
                mock.request()
                require(cron_run["status"] == "completed" and cron_run["threadId"] not in [chain, suppressed], "manual cron failed to create an independent chat")
                require(app.rpc("scheduler/read", {"id": cron["id"]})["task"]["state"] == "paused", "Run now activated a paused schedule")
                require(cron_run["summary"] == "Manual cron completed" and run_notice(app, cron_run["id"])["shouldNotify"] is True, "scheduled_task_report did not control summary/notification")
                require(cron_run.get("hasUpdate") is True, "scheduled_task_report.hasUpdate was not persisted in the run record")
                no_internal_tools_in_timeline(app, cron_run["threadId"])
                passed("paused cron Run now executes an independent Agent chat and saves task report")

                target = app.rpc("thread/start", {"mode": "work"})["thread"]["id"]
                heartbeat = create_task(app, workspace, "HEARTBEAT_CONTEXT", kind="heartbeat", targetThreadId=target)
                expect_error(app, "scheduler/update", {"id": heartbeat["id"], "autoArchive": True}, "heartbeat auto-archive guard")
                heartbeat_runs = []
                for summary, update in [("No meaningful change", False), ("A meaningful update", True)]:
                    mock.report(summary, update)
                    claim = app.rpc("scheduler/run", {"id": heartbeat["id"]})
                    row = wait_run(app, heartbeat["id"], claim["run"]["id"])
                    body = mock.request()
                    heartbeat_runs.append(row)
                    require(row["threadId"] == target and row["status"] == "completed", "heartbeat did not return to its existing chat")
                    require(row.get("hasUpdate") is update, "heartbeat report.hasUpdate was lost from the persisted run")
                    require(run_notice(app, row["id"])["shouldNotify"] is update, "important-updates notification policy is wrong")
                    if update:
                        require(sum(i.get("role") == "user" for i in body["input"]) == 2, "heartbeat lost existing conversation context")
                require(app.rpc("thread/read", {"threadId": target})["thread"].get("archived") is not True, "heartbeat archived its main conversation")
                app.rpc("scheduler/update", {"id": heartbeat["id"], "notificationPolicy": "failed_runs_only"})
                mock.report("A silent successful update", True)
                claim = app.rpc("scheduler/run", {"id": heartbeat["id"]})
                row = wait_run(app, heartbeat["id"], claim["run"]["id"])
                mock.request()
                require(run_notice(app, row["id"])["shouldNotify"] is False, "failed-only policy notified on success")
                mock.fail()
                claim = app.rpc("scheduler/run", {"id": heartbeat["id"]})
                failed_heartbeat = wait_run(app, heartbeat["id"], claim["run"]["id"])
                mock.request()
                require(failed_heartbeat["status"] == "failed" and run_notice(app, failed_heartbeat["id"])["shouldNotify"] is True, "failed-only policy failed to report an actual error")
                passed("heartbeat context, no-update/important-update decisions, failed-only notifications, and archive guard")

                prior_runs = {record["id"] for record in app.rpc("scheduler/runs", {"taskId": heartbeat["id"]})["data"]}
                app.rpc("thread/goal/set", {"threadId": target, "objective": "GOAL_DELETE_ASSOCIATIONS keep bound product state consistent", "status": "paused"})
                app.rpc("scheduler/update", {"id": heartbeat["id"], "state": "active", "schedule": schedule_after(2)})
                app.rpc("thread/delete", {"threadId": target})
                require(goal(app, target) is None, "deleting a native chat left its bound Goal behind")
                retained_task = app.rpc("scheduler/read", {"id": heartbeat["id"]})["task"]
                require(retained_task["state"] == "paused" and retained_task["targetThreadId"] == target, "deleting a chat did not pause and retain its heartbeat definition")
                expect_error(app, "thread/read", {"threadId": target}, "deleted native chat read")
                request_count = len(mock.history)
                deadline = time.monotonic() + 2.5
                while time.monotonic() < deadline:
                    records = app.rpc("scheduler/runs", {"taskId": heartbeat["id"]})["data"]
                    require({record["id"] for record in records} == prior_runs, "deleted heartbeat target lost history or produced a new orphan run")
                    require(len(mock.history) == request_count, "deleting a native chat triggered new model work")
                    time.sleep(0.06)
                passed("native chat deletion clears its Goal, pauses heartbeat, and preserves definition/history without orphan runs")

                mock.fail()
                due = create_task(app, workspace, "AUTOMATIC_DUE_FAILURE", state="active", schedule=schedule_after(2))
                due_run = wait_run(app, due["id"])
                mock.request()
                exhausted = app.rpc("scheduler/read", {"id": due["id"]})["task"]
                require(due_run["trigger"] == "scheduled" and due_run["status"] == "failed" and due_run.get("error"), "a real automatic due run did not retain API failure")
                require(exhausted["state"] == "completed" and exhausted["nextRunAt"] is None, "COUNT-exhausted calendar was not completed")
                require(due_run["status"] != exhausted["state"], "calendar exhaustion was mistaken for successful execution")
                passed("real future DTSTART dispatch and failed COUNT-exhausted run")

                repo = root / "repository"
                repo.mkdir()
                git_env = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull}
                for args in [["init", "-q", str(repo)], ["-C", str(repo), "add", "."], ["-C", str(repo), "-c", "user.name=Local Smoke", "-c", "user.email=smoke@example.invalid", "-c", "core.hooksPath=" + os.devnull, "commit", "-q", "--allow-empty", "-m", "fixture"]]:
                    subprocess.run(["git", *args], env=git_env, check=True, capture_output=True, timeout=timeout)
                mock.shell("worktree-artifact.txt", 5)
                mock.report("Worktree artifact verified", True)
                worktree = create_task(app, repo, "WORKTREE_EXECUTION", executionEnvironment="worktree")
                claim = app.rpc("scheduler/run", {"id": worktree["id"]})
                worktree_run = wait_run(app, worktree["id"], claim["run"]["id"], approve=True)
                mock.request()
                mock.request()
                require(worktree_run["status"] == "completed" and worktree_run["worktrees"], f"worktree execution failed: {worktree_run}")
                worktree_path = Path(worktree_run["worktrees"][0]["cwd"]).resolve()
                require(worktree_path.is_relative_to(root) and (worktree_path / "worktree-artifact.txt").read_text() == "fixture-work", "Agent did not execute inside its isolated worktree")
                require(not (repo / "worktree-artifact.txt").exists(), "scheduled worktree changed the original checkout")
                managed_id = worktree_run["worktrees"][0].get("managedId")
                managed = app.rpc("workspace/worktree/list", {"cwd": str(repo)})["data"]
                require(managed_id and any(tree.get("id") == managed_id and tree.get("managed") for tree in managed), "scheduled worktree is missing from recoverable managed workspaces")
                passed("real Git worktree runs Agent shell and retains its result artifact")

                app.rpc("scheduler/run/update", {"id": cron_run["id"], "reviewState": "ACCEPTED", "unread": False})
                app.rpc("scheduler/run/update", {"id": failed_heartbeat["id"], "reviewState": "ARCHIVED", "unread": False})
                snapshot = app.rpc("scheduler/runs")["data"]
                count_before_restart = len(mock.history)
                rpc_total += app.rpc_count
                app.close()
                app = AppServer(binary, store, workspace, mock.base_url, timeout)
                initialize(app, store)
                require(goal(app, chain)["tokensUsed"] == 35 and goal(app, chain)["status"] == "complete", "completed goal did not survive restart")
                require(goal(app, edited)["tokensUsed"] == 5 and goal(app, edited)["status"] == "paused", "edited paused goal did not survive restart")
                require(goal(app, replacement)["tokensUsed"] == 0 and goal(app, cleared) is None, "replaced/cleared goals changed on restart")
                require(goal(app, target) is None and app.rpc("scheduler/read", {"id": heartbeat["id"]})["task"]["state"] == "paused", "deleted chat association cleanup did not survive restart")
                assert_user_message_flags(app, chain, [False, True], live=False)
                assert_user_message_flags(app, queued_goal, [False, False], live=False)
                restored = app.rpc("scheduler/runs")["data"]
                require({row["id"] for row in restored} == {row["id"] for row in snapshot}, "scheduled runs disappeared or duplicated after restart")
                by_id = {row["id"]: row for row in restored}
                require(by_id[cron_run["id"]]["reviewState"] == "ACCEPTED" and not by_id[cron_run["id"]]["unread"], "accepted review state did not survive restart")
                require(by_id[cron_run["id"]].get("hasUpdate") is True and by_id[heartbeat_runs[0]["id"]].get("hasUpdate") is False, "hasUpdate report state did not survive restart")
                require(by_id[failed_heartbeat["id"]]["reviewState"] == "ARCHIVED", "archived review state did not survive restart")
                require(app.rpc("scheduler/read", {"id": cron["id"]})["task"]["state"] == "paused", "restart activated a paused task")
                assert_quiet(app, mock, "terminal/paused state after restart")
                require(len(mock.history) == count_before_restart, "restart reran settled work")
                require(not mock.errors and mock.plans.empty() and mock.requests.empty(), f"mock plans/requests were left unresolved: {mock.errors}")
                passed("restart preserves goals, task definitions, run lifecycle, and read/review state")
                rpc_total += app.rpc_count
            finally:
                app.close()
    print(f"PASS automations smoke: {checks} checks, {rpc_total} RPCs, {len(mock.history)} local Responses requests; no external model calls; temporary data removed", flush=True)


def main() -> int:
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=8)
    options = parser.parse_args()
    require(0 < options.timeout <= 10, "timeout must be greater than 0 and at most 10 seconds")
    require(options.binary.is_file(), "build target/debug/mcodex before running this smoke test")
    try:
        run(options.binary.resolve(), options.timeout)
        return 0
    except Exception as error:
        print(f"FAIL automations smoke: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
