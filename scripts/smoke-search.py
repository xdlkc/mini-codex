#!/usr/bin/env python3
"""Real Rust hosted-search turns against a loopback-only Responses provider.

Temporary allowlisted HOME/MCODEX_HOME and fixture credentials only. No real
search engine, paid API, installed app account, or external website is contacted.
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
import sys
import tempfile
import threading
import time
from typing import Any

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("search_account_helpers", Path(__file__).with_name("smoke-account.py"))
assert SPEC and SPEC.loader
ACCOUNT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ACCOUNT
SPEC.loader.exec_module(ACCOUNT)
AppServer, SmokeFailure, require = ACCOUNT.AppServer, ACCOUNT.SmokeFailure, ACCOUNT.require
KEY = ACCOUNT.ENV_KEY
ACCESS = "e30." + base64.urlsafe_b64encode(json.dumps({"exp": 4_000_000_000}).encode()).decode().rstrip("=") + ".unused-search-signature"
REFRESH = "unused-search-fixture-refresh"
SECRETS = ACCOUNT.ALL_KEYS + [ACCESS, REFRESH]
SOURCE = "https://search-fixture.invalid/cited-page"
SECOND = "https://search-fixture.invalid/consulted-only"
THIRD = "https://other-search-fixture.invalid/extra"
URLS = [SOURCE, SECOND, THIRD]
TEXTS = ["中文🙂结论一，引用原始资料。", "第二段仍引用同一个来源。"]
MODEL = "local-mock-model"


def public(value: Any) -> None:
    text = json.dumps(value, ensure_ascii=False)
    require(all(secret not in text for secret in SECRETS), "a search RPC, event, or error exposed a fixture credential")


@dataclass
class Plan:
    number: int
    mode: str
    search: str
    kind: str = "answer"
    key: str = KEY
    account_id: str | None = None
    choice: str = "auto"
    text: str = "普通答复没有声称进行了搜索。"
    opened: threading.Event = field(default_factory=threading.Event)
    release: threading.Event = field(default_factory=threading.Event)
    finished: threading.Event = field(default_factory=threading.Event)

    @property
    def provider_id(self) -> str:
        return f"ws_fixture_{self.number}"

    @property
    def message_id(self) -> str:
        return f"msg_fixture_{self.number}"


def web_call(plan: Plan, status: str = "completed") -> dict[str, Any]:
    return {"type": "web_search_call", "id": plan.provider_id, "status": status, "action": {"type": "search", "queries": ["fixture current facts", "中文检索"], "sources": [{"type": "url", "url": url} for url in URLS]}}


def answer_texts(plan: Plan) -> list[str]:
    return [text + f"（fixture {plan.number}）" for text in TEXTS]


def cited_message(plan: Plan, citations: bool = True) -> dict[str, Any]:
    parts = []
    for index, text in enumerate(answer_texts(plan)):
        # These are provider integers, deliberately left in provider units.
        # The smoke must not manufacture a JavaScript UTF-16 conversion contract.
        annotation = {"type": "url_citation", "start_index": 2 + index, "end_index": 7 + index, "url": SOURCE, "title": "中文资料 🙂"}
        parts.append({"type": "output_text", "text": text, "annotations": [annotation] if citations else []})
    return {"type": "message", "id": plan.message_id, "role": "assistant", "status": "completed", "content": parts}


def plain_message(plan: Plan, *, reference: bool = False) -> dict[str, Any]:
    part: dict[str, Any] = {"type": "output_text", "text": plan.text, "annotations": []}
    if reference:
        part["annotations"] = [{"type": "url_citation", "start_index": 0, "end_index": 2, "url": SOURCE, "title": "之前已使用的来源"}]
    return {"type": "message", "id": plan.message_id, "role": "assistant", "status": "completed", "content": [part]}


def multi_texts(plan: Plan) -> list[str]:
    return [f"开头中文🙂｜末尾A｜最后A（{plan.number}）", f"匿名中文🙂｜末尾B｜最后B（{plan.number}）", f"相同中文🙂文本（{plan.number}）", f"相同中文🙂文本（{plan.number}）"]


def partial_text(plan: Plan) -> str:
    return f"已观察的未完成中文🙂（{plan.number}）｜最后一段仍在生成"


def partial_query(plan: Plan) -> str:
    return f"observed-partial-query-{plan.number}"


class Provider:
    def __init__(self, timeout: float):
        self.timeout = timeout
        self.plans: queue.Queue[Plan] = queue.Queue()
        self.requests: queue.Queue[dict[str, Any]] = queue.Queue()
        self.history: list[dict[str, Any]] = []
        self.all_plans: list[Plan] = []
        self.errors: list[str] = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_: Any) -> None:
                pass

            def do_POST(self) -> None:
                plan = None
                self.connection.settimeout(owner.timeout)
                try:
                    require(self.path == "/v1/responses", "hosted search called an unexpected local endpoint")
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size <= 16 * 1024 * 1024, "fixture request size exceeds its bound")
                    body = json.loads(self.rfile.read(size))
                    authorization = self.headers.get("Authorization", "")
                    require(authorization in ["Bearer " + KEY, "Bearer " + ACCESS], "provider received credentials outside the isolated fixtures")
                    record = {"key": authorization[7:], "body": body}
                    # Log arrival before waiting for a plan: an unintended retry must be visible.
                    owner.history.append(record)
                    plan = owner.plans.get(timeout=owner.timeout)
                    require(authorization == "Bearer " + plan.key, "search turn used the wrong credential")
                    require(self.headers.get("chatgpt-account-id") == plan.account_id, "search turn used the wrong OAuth account header")
                    require(body.get("model") == MODEL and body.get("store") is False, "search turn lost its configured model or local history policy")
                    tools = body.get("tools", [])
                    hosted = [tool for tool in tools if tool.get("type") == "web_search"]
                    require(len(hosted) == (0 if plan.search == "disabled" else 1), "hosted search tool declaration does not match effective policy")
                    require(not any(tool.get("type") == "web_search_preview" for tool in tools), "a deprecated preview tool was silently substituted")
                    include = body.get("include", [])
                    require(("web_search_call.action.sources" in include) is (plan.search != "disabled"), "full source inclusion does not match effective search policy")
                    if hosted:
                        require(hosted[0].get("external_web_access", True) is (plan.search != "cached"), "cached/live search changed before reaching the provider")
                    functions = {tool.get("name") for tool in tools if tool.get("type") == "function"}
                    if plan.mode == "chat":
                        require(not functions, "Chat mode exposed sandbox shell or local function tools")
                    else:
                        require({"shell", "apply_patch"} <= functions, "Work/Codex lost local tools when hosted search was configured")
                    expected_choice: Any = {"type": "web_search"} if plan.choice == "required" else "auto"
                    require(body.get("tool_choice") == expected_choice, "manual search did not force only its first request, or a later loop remained forced")
                    require("_mcodex_internal" not in json.dumps(body), "private app history marker leaked into the provider request")
                    owner.requests.put(record)
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Transfer-Encoding", "chunked")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    if plan.kind.startswith("unauthorized-"):
                        kind = plan.kind.split("-", 1)[1]
                        if kind == "progress":
                            self.event({"type": "response.web_search_call.searching", "item_id": plan.provider_id, "output_index": 0, "sequence_number": 0})
                        elif kind in ["added", "done"]:
                            self.event({"type": "response.output_item." + kind, "output_index": 0, "item": web_call(plan)})
                        else:
                            self.complete([web_call(plan), plain_message(plan)])
                    elif plan.kind == "multi":
                        self.multi(plan)
                    elif plan.kind in ["partial-interrupted", "partial-failed"]:
                        call = {"type": "web_search_call", "id": plan.provider_id, "status": "searching", "action": {"type": "search", "queries": [partial_query(plan)], "sources": []}}
                        self.event({"type": "response.output_item.added", "output_index": 0, "item": call})
                        self.event({"type": "response.web_search_call.searching", "output_index": 0, "item_id": plan.provider_id, "sequence_number": 1})
                        message: dict[str, Any] = {"type": "message", "role": "assistant", "status": "in_progress", "content": []}
                        if plan.kind == "partial-failed":
                            message["id"] = plan.message_id
                        self.event({"type": "response.output_item.added", "output_index": 1, "item": message})
                        identity = {"item_id": plan.message_id} if "id" in message else {}
                        self.event_unicode({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": f"已观察的未完成中文🙂（{plan.number}）", **identity})
                        self.event({"type": "response.output_item.added", "output_index": 2, "item": {"type": "reasoning", "id": f"rs_partial_{plan.number}", "summary": []}})
                        self.event({"type": "response.reasoning_summary_text.delta", "output_index": 2, "item_id": f"rs_partial_{plan.number}", "summary_index": 0, "delta": "可公开的部分推理摘要"})
                        self.event({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "｜最后一段仍在生成", **identity})
                        plan.opened.set()
                        require(plan.release.wait(owner.timeout), "partial UI fixture was not released before its deadline")
                        if plan.kind == "partial-failed":
                            self.event({"type": "response.failed", "response": {"status": "failed", "error": {"message": "isolated failure after observed partial text"}}})
                        else:
                            self.complete([web_call(plan), plain_message(plan)])
                    elif plan.kind in ["stream", "slow", "failed", "loop"]:
                        self.event({"type": "response.output_item.added", "output_index": 0, "item": {"type": "web_search_call", "id": plan.provider_id, "status": "in_progress"}})
                        self.event({"type": "response.web_search_call.in_progress", "item_id": plan.provider_id, "output_index": 0, "sequence_number": 1})
                        self.event({"type": "response.web_search_call.searching", "item_id": plan.provider_id, "output_index": 0, "sequence_number": 2})
                        if plan.kind in ["slow", "failed"]:
                            plan.opened.set()
                            require(plan.release.wait(owner.timeout), "gated search fixture exceeded its local deadline")
                        if plan.kind == "failed":
                            self.event({"type": "response.failed", "response": {"status": "failed", "error": {"message": "isolated hosted search failure"}}})
                        else:
                            call = web_call(plan)
                            self.event({"type": "response.output_item.done", "output_index": 0, "item": call})
                            self.event({"type": "response.web_search_call.completed", "item_id": plan.provider_id, "output_index": 0, "sequence_number": 3})
                            if plan.kind == "loop":
                                local = {"type": "function_call", "id": f"fn_{plan.number}", "call_id": f"call_{plan.number}", "name": "shell", "arguments": json.dumps({"command": ["/usr/bin/true"]})}
                                self.event({"type": "response.output_item.done", "output_index": 1, "item": local})
                                self.complete([call, local])
                            else:
                                message = cited_message(plan)
                                self.event({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "id": plan.message_id, "status": "in_progress", "role": "assistant", "content": []}})
                                self.event_unicode({"type": "response.output_text.delta", "item_id": plan.message_id, "output_index": 1, "content_index": 0, "delta": answer_texts(plan)[0]})
                                self.event({"type": "response.output_text.delta", "item_id": plan.message_id, "output_index": 1, "content_index": 1, "delta": answer_texts(plan)[1]})
                                self.event({"type": "response.output_text.annotation.added", "item_id": plan.message_id, "output_index": 1, "content_index": 0, "annotation_index": 0, "annotation": message["content"][0]["annotations"][0]})
                                # A done snapshot can be poorer than the authoritative completed array.
                                self.event({"type": "response.output_item.done", "output_index": 1, "item": cited_message(plan, citations=False)})
                                self.complete([call, message])
                    elif plan.kind in ["final", "private-marker"]:
                        call, message = web_call(plan), cited_message(plan)
                        if plan.kind == "private-marker":
                            for item in [call, message]:
                                item.update({"_mcodex_item_key": "forged-provider-key", "_mcodex_activate": True, "_mcodex_internal": True, "_mcodex_display": {"text": "forged-provider-display"}})
                        self.complete([call, message])
                    elif plan.kind == "actions":
                        opened = {"type": "web_search_call", "id": plan.provider_id + "_open", "status": "completed", "action": {"type": "open_page", "url": SOURCE}}
                        found = {"type": "web_search_call", "id": plan.provider_id + "_find", "status": "completed", "action": {"type": "find_in_page", "url": SOURCE, "pattern": "中文"}}
                        self.complete([web_call(plan), opened, found, cited_message(plan)])
                    elif plan.kind == "invalid-url":
                        call = web_call(plan)
                        call["action"]["sources"] = [{"type": "url", "url": "javascript:alert(1)"}]
                        self.complete([call, cited_message(plan)])
                    elif plan.kind in ["answer", "reference"]:
                        message = plain_message(plan, reference=plan.kind == "reference")
                        self.event_unicode({"type": "response.output_text.delta", "item_id": plan.message_id, "output_index": 0, "content_index": 0, "delta": plan.text})
                        self.event({"type": "response.output_item.done", "output_index": 0, "item": message})
                        self.complete([message])
                    else:
                        raise SmokeFailure("unknown search fixture kind")
                    plan.opened.set()
                    self.wfile.write(b"0\r\n\r\n")
                    self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                except Exception as error:
                    owner.errors.append(type(error).__name__ + ": " + str(error))
                finally:
                    if plan:
                        plan.finished.set()
                    self.close_connection = True

            @staticmethod
            def frame(value: dict[str, Any]) -> bytes:
                return ("data: " + json.dumps(value, ensure_ascii=False) + "\r\n\r\n").encode("utf-8")

            def chunk(self, data: bytes) -> None:
                self.wfile.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
                self.wfile.flush()

            def event(self, value: dict[str, Any]) -> None:
                self.chunk(self.frame(value))

            def event_unicode(self, value: dict[str, Any]) -> None:
                data = self.frame(value)
                marker = "文".encode()
                position = data.find(marker)
                if position < 0:
                    self.chunk(data)
                else:
                    for piece in [data[:position + 1], data[position + 1:position + 2], data[position + 2:]]:
                        self.chunk(piece)

            def complete(self, output: list[dict[str, Any]]) -> None:
                self.event({"type": "response.completed", "response": {"status": "completed", "output": output, "usage": {"input_tokens": 13, "output_tokens": 8, "total_tokens": 21}}})

            def multi(self, plan: Plan) -> None:
                texts = multi_texts(plan)
                messages = []
                for index, text in enumerate(texts):
                    item: dict[str, Any] = {"type": "message", "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": text, "annotations": []}]}
                    if index in [0, 2]:
                        item["id"] = plan.message_id + ("_a" if index == 0 else "_same")
                    messages.append(item)
                a, b, same_named, same_anon = messages
                reasoning_id = f"rs_multi_{plan.number}"
                reasoning = {"type": "reasoning", "id": reasoning_id, "summary": [{"type": "summary_text", "text": "多消息之间的公开摘要"}]}
                self.event({"type": "response.output_item.added", "output_index": 0, "item": {"type": "message", "id": a["id"], "role": "assistant", "status": "in_progress", "content": []}})
                self.event_unicode({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": a["id"], "delta": "开头中文🙂"})
                self.event({"type": "response.output_item.added", "output_index": 3, "item": {"type": "reasoning", "id": reasoning_id, "summary": []}})
                self.event({"type": "response.reasoning_summary_text.delta", "output_index": 3, "item_id": reasoning_id, "summary_index": 0, "delta": "多消息之间的公开摘要"})
                self.event({"type": "response.output_item.added", "output_index": 2, "item": {"type": "web_search_call", "id": plan.provider_id, "status": "in_progress"}})
                self.event({"type": "response.web_search_call.searching", "output_index": 2, "item_id": plan.provider_id, "sequence_number": 1})
                self.event({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": a["id"], "delta": "｜末尾A"})
                self.event({"type": "response.output_item.added", "output_index": 1, "item": {"type": "message", "role": "assistant", "status": "in_progress", "content": []}})
                self.event_unicode({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "匿名中文🙂"})
                self.event({"type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": a["id"], "delta": f"｜最后A（{plan.number}）"})
                self.event({"type": "response.output_item.done", "output_index": 0, "item": a})
                self.event({"type": "response.output_item.done", "output_index": 2, "item": web_call(plan)})
                self.event({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": "｜末尾B"})
                self.event({"type": "response.output_item.done", "output_index": 3, "item": reasoning})
                self.event({"type": "response.output_text.delta", "output_index": 1, "content_index": 0, "delta": f"｜最后B（{plan.number}）"})
                plan.opened.set()
                require(plan.release.wait(owner.timeout), "multi-message active-state fixture was not released")
                self.event({"type": "response.output_item.done", "output_index": 1, "item": b})
                for index, item in [(4, same_named), (5, same_anon)]:
                    added = {"type": "message", "role": "assistant", "status": "in_progress", "content": []}
                    identity = {"item_id": item["id"]} if "id" in item else {}
                    if "id" in item:
                        added["id"] = item["id"]
                    self.event({"type": "response.output_item.added", "output_index": index, "item": added})
                    self.event_unicode({"type": "response.output_text.delta", "output_index": index, "content_index": 0, "delta": item["content"][0]["text"], **identity})
                    self.event({"type": "response.output_item.done", "output_index": index, "item": item})
                for item in [a, b]:
                    item["content"][0]["annotations"] = [{"type": "url_citation", "url": SOURCE, "title": "多消息真实引用", "start_index": 2, "end_index": 7}]
                self.complete([a, b, web_call(plan), reasoning, same_named, same_anon])

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}/v1"
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=0.05), daemon=True)
        self.thread.start()

    def plan(self, mode: str = "chat", search: str = "auto", **fields: Any) -> Plan:
        result = Plan(len(self.all_plans) + 1, mode, search, **fields)
        self.all_plans.append(result)
        self.plans.put(result)
        return result

    def request(self) -> dict[str, Any]:
        try:
            return self.requests.get(timeout=self.timeout)
        except queue.Empty as error:
            raise SmokeFailure("loopback provider received no matching request; " + "; ".join(self.errors)) from error

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
        text = str(error)
        public(text)
        require("RPC error:" in text, "expected search rejection failed through transport instead of JSONL RPC")
        return text
    raise SmokeFailure("an invalid search preference unexpectedly succeeded")


def start_turn(app: AppServer, thread_id: str, **fields: Any) -> str:
    return rpc(app, "turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": "Isolated web search fixture; use only the requested tools."}], "reasoningEffort": "auto", **fields})["turn"]["id"]


def completed(app: AppServer, thread_id: str, turn_id: str, status: str = "completed") -> dict[str, Any]:
    result = app.event("turn/completed", thread_id, turn_id)
    require(result["turn"]["status"] == status, "turn did not reach its expected terminal status")
    public(app.notifications)
    return result["turn"]


def thread(app: AppServer, thread_id: str) -> dict[str, Any]:
    return rpc(app, "thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]


def item_events(app: AppServer, thread_id: str, turn_id: str, kind: str | None = None) -> list[dict[str, Any]]:
    result = []
    for event in app.notifications:
        params = event.get("params", {})
        item = params.get("item")
        if params.get("threadId") == thread_id and params.get("turnId") == turn_id and isinstance(item, dict) and (kind is None or item.get("type") == kind):
            result.append(item)
    return result


def all_items(value: dict[str, Any]) -> list[dict[str, Any]]:
    return [item for turn in value.get("turns", []) for item in turn.get("items", [])]


def search_items(value: dict[str, Any]) -> list[dict[str, Any]]:
    return [item for item in all_items(value) if item.get("type") == "webSearch"]


def cited_answer(value: dict[str, Any], plan: Plan) -> dict[str, Any]:
    texts = answer_texts(plan)
    matches = [item for item in all_items(value) if item.get("type") == "agentMessage" and (item.get("providerId") == plan.message_id or item.get("providerId") is None and item.get("text") == "".join(texts))]
    require(len(matches) == 1, "richer done/final snapshots created duplicate answers or lost the authoritative message")
    result = matches[0]
    require(result["text"] == "".join(texts) and len(result.get("parts", [])) == 2, "multi-part Chinese/emoji answer text was corrupted")
    for index, part in enumerate(result["parts"]):
        require(part["type"] == "output_text" and part["text"] == texts[index], "message part text changed")
        links = part.get("citations", [])
        require(len(links) == 1, "authoritative final citation annotation was lost")
        citation = links[0]
        require(citation["type"] == "url_citation" and citation["url"] == SOURCE and citation["title"] == "中文资料 🙂", "citation URL/title was lost")
        require(citation["startIndex"] == 2 + index and citation["endIndex"] == 7 + index and citation["indexUnit"] == "provider", "citation offsets were guessed as UTF-16 or rewritten")
    require(len(result.get("citations", [])) == 2, "part citations did not reach the aggregated view")
    return result


def verify_sources(value: dict[str, Any], plan: Plan) -> dict[str, Any]:
    matches = [item for item in search_items(value) if item.get("providerId") == plan.provider_id]
    require(len(matches) == 1, "one provider search call became duplicate activity rows")
    call = matches[0]
    require(call["status"] == "completed" and call["actionType"] == "search" and call["queries"] == ["fixture current facts", "中文检索"], "completed search activity lost its action or queries")
    require([source["url"] for source in call["sources"]] == URLS, "consulted sources were replaced by the smaller citation set")
    return call


def no_new_search(app: AppServer, thread_id: str, turn_id: str) -> None:
    require(not item_events(app, thread_id, turn_id, "webSearch"), "a disabled/plain response fabricated a hosted-search activity")


def message_timeline(app: AppServer, thread_id: str, turn_id: str) -> dict[str, dict[str, Any]]:
    """Apply real wire notifications with the same completed-before-delta boundary as UI consumers."""
    messages: dict[str, dict[str, Any]] = {}
    for event in app.notifications:
        params = event.get("params", {})
        if params.get("threadId") != thread_id or params.get("turnId") != turn_id:
            continue
        item = params.get("item", {})
        if item.get("type") == "agentMessage":
            item_id = item["id"]
            state = messages.setdefault(item_id, {"text": "", "closed": False, "item": {}})
            if event.get("method") == "item/completed":
                state.update(text=item.get("text", ""), closed=True, item=item)
        elif event.get("method") == "item/agentMessage/delta":
            item_id = params["itemId"]
            require(item_id in messages, "text delta arrived without a real started message")
            state = messages[item_id]
            require(not state["closed"], "an active message was completed before its later Unicode text delta")
            state["text"] += params["delta"]
    return messages


def verify_multi(value: dict[str, Any], plan: Plan) -> None:
    messages = [item for item in all_items(value) if item.get("type") == "agentMessage"]
    expected = multi_texts(plan)
    require(len(messages) == 4 and len({item["id"] for item in messages}) == 4, "anonymous/named messages were merged or richer snapshots created extra answers")
    require(sorted(item["text"] for item in messages) == sorted(expected), "interleaved message text lost a prefix, emoji or last delta")
    require(sum(item["text"] == expected[2] for item in messages) == 2, "two identical texts from distinct provider messages were deduplicated")
    for text in expected[:2]:
        item = next(item for item in messages if item["text"] == text)
        citations = item.get("parts", [{}])[0].get("citations", [])
        require(len(citations) == 1 and citations[0]["url"] == SOURCE and citations[0]["indexUnit"] == "provider", "richer final metadata did not reach the same completed message")
    verify_sources(value, plan)


def verify_partial(value: dict[str, Any], plan: Plan, status: str) -> None:
    matches = [turn for turn in value.get("turns", []) if any(item.get("type") == "webSearch" and item.get("providerId") == plan.provider_id for item in turn.get("items", []))]
    require(len(matches) == 1 and matches[0]["status"] == status, "observed failed/interrupted search turn was lost, duplicated or restored as successful")
    items = matches[0]["items"]
    calls = [item for item in items if item.get("type") == "webSearch"]
    messages = [item for item in items if item.get("type") == "agentMessage"]
    require(len(calls) == 1 and calls[0]["status"] == status and calls[0]["queries"] == [partial_query(plan)], "observed partial search metadata or terminal status was not retained")
    require(calls[0]["sources"] == [], "incomplete search invented consulted sources")
    require(len(messages) == 1 and messages[0]["text"] == partial_text(plan), "observed partial answer lost text around reasoning/search events or became duplicate messages")
    require(messages[0].get("status") in ["failed", "interrupted", "incomplete"], "partial answer was presented as a complete model message")
    require(sum(item.get("type") == "userMessage" for item in items) == 1, "UI snapshot duplicated the real user message")
    require(len({item["id"] for item in items}) == len(items), "restored UI items reused an ambiguous identity")
    public(value)


def verify_model_has_no_ui_snapshot(body: dict[str, Any], plan: Plan) -> None:
    def walk(value: Any) -> None:
        if isinstance(value, dict):
            require(not any(key == "uiTurn" or key.startswith("_mcodex_") for key in value), "private UI snapshot wrapper or control field entered model input")
            require(value.get("type") not in ["webSearch", "agentMessage", "web_search_call"], "partial UI was serialized as a synthetic successful model output")
            for child in value.values():
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)
    walk(body)
    encoded = json.dumps(body, ensure_ascii=False)
    require(partial_text(plan) not in encoded and partial_query(plan) not in encoded and "可公开的部分推理摘要" not in encoded, "display-only partial text, query or reasoning leaked into model history")


def run(binary: Path, timeout: float) -> None:
    checks, rpc_count = 0, 0
    live: list[AppServer] = []

    def passed(label: str) -> None:
        nonlocal checks
        checks += 1
        print("PASS " + label, flush=True)

    def stop(app: AppServer) -> None:
        nonlocal rpc_count
        if app in live:
            live.remove(app)
            rpc_count += app.rpc_count
            app.close()
            public(list(app.stderr))

    def eof_stop(app: AppServer) -> None:
        require(app.process.stdin is not None, "app-server stdin is unavailable for EOF verification")
        app.process.stdin.close()
        app.process.wait(timeout=timeout)
        require(app.process.returncode == 0, "app-server did not exit cleanly after EOF with saved UI state")
        stop(app)

    def mode(value: dict[str, Any], preferred: str, effective: str) -> None:
        require(value.get("webSearchMode") == preferred, "preferred search mode was lost or replaced by its resolved mode")
        require(value.get("effectiveWebSearchMode") == effective, "effective search mode ignored app mode, configuration or policy")
        require(value.get("webSearch") is (effective != "disabled"), "legacy search bool is not derived from effective policy")

    with tempfile.TemporaryDirectory(prefix="mcodex-search-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace, fixture_home = root / "own", root / "workspace", root / "home"
        workspace.mkdir(); fixture_home.mkdir(); store.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        with Provider(timeout) as provider:
            def start(path: Path = store, *, credential: str | None = KEY) -> AppServer:
                app = AppServer(binary, path, workspace, fixture_home, provider.base_url, timeout, env_key=credential)
                live.append(app)
                ACCOUNT.initialize(app, path)
                return app

            def new(app: AppServer, app_mode: str = "chat", preferred: str | None = None, **fields: Any) -> dict[str, Any]:
                return rpc(app, "thread/start", {"mode": app_mode, **({"webSearchMode": preferred} if preferred is not None else {}), **fields})["thread"]

            def set_mode(app: AppServer, thread_id: str, preferred: str) -> dict[str, Any]:
                return rpc(app, "thread/webSearch/set", {"threadId": thread_id, "mode": preferred})

            def answer(app: AppServer, thread_id: str, app_mode: str, effective: str, **fields: Any) -> tuple[str, Plan, dict[str, Any]]:
                plan = provider.plan(app_mode, effective)
                turn_id = start_turn(app, thread_id, **fields)
                wire = provider.request()
                completed(app, thread_id, turn_id)
                no_new_search(app, thread_id, turn_id)
                return turn_id, plan, wire

            try:
                app = start()
                caps = rpc(app, "web/search/capabilities")
                require(caps.get("available") is True and caps.get("permissionsVerified") is False and caps.get("policy", {}).get("allowed") is True, "connected capability falsely proved provider entitlement or lost policy")
                require(caps["defaultModes"] == {"chat": "auto", "work": "auto", "codex": "cached"}, "default modes do not match the official-backed local defaults")
                full = rpc(app, "web/search/capabilities", {"sandboxMode": "danger-full-access"})
                require(full["defaultModes"]["codex"] == "live", "full-access Codex did not default to live search")
                unsigned = start(root / "signed-out", credential=None)
                cap = rpc(unsigned, "web/search/capabilities")
                require(cap["available"] is False and cap["permissionsVerified"] is False, "signed-out search was advertised as connected")
                stop(unsigned)
                require(not provider.history, "capability reads contacted an external model")
                rejected(app, "desktop/settings/update", {"preferences": {"webSearchAllowed": "false"}})
                require(rpc(app, "web/search/capabilities")["policy"]["allowed"] is True, "invalid global policy corrupted or enabled search settings")
                passed("capabilities distinguish connected account, unknown entitlement, defaults and full-access policy")

                defaults: dict[str, dict[str, Any]] = {}
                for app_mode, effective in [("chat", "auto"), ("work", "auto"), ("codex", "cached")]:
                    created = new(app, app_mode)
                    mode(created, "auto", effective)
                    defaults[app_mode] = created
                    mode(thread(app, created["id"]), "auto", effective)
                    answer(app, created["id"], app_mode, effective)
                listed = rpc(app, "thread/list")["data"]
                for app_mode, created in defaults.items():
                    found = next(item for item in listed if item["id"] == created["id"])
                    mode(found, "auto", "cached" if app_mode == "codex" else "auto")
                passed("new Chat/Work Auto and Codex cached declarations retain proper local-tool separation")

                chat = defaults["chat"]["id"]
                before = len(provider.history)
                for invalid in ["unknown", 1, None]:
                    rejected(app, "thread/webSearch/set", {"threadId": chat, "mode": invalid})
                    rejected(app, "turn/start", {"threadId": chat, "webSearchMode": invalid, "input": [{"type": "text", "text": "invalid mode must not run"}]})
                rejected(app, "thread/start", {"mode": "chat", "webSearchMode": "unknown"})
                rejected(app, "thread/webSearch/set", {"threadId": chat, "enabled": "false"})
                require(len(provider.history) == before, "invalid search preference sent a paid request")
                mode(thread(app, chat), "auto", "auto")
                mode(rpc(app, "thread/webSearch/set", {"threadId": chat, "enabled": False}), "disabled", "disabled")
                answer(app, chat, "chat", "disabled")
                mode(rpc(app, "thread/webSearch/set", {"threadId": chat, "enabled": True}), "auto", "auto")
                answer(app, chat, "chat", "auto")
                legacy = new(app, "chat", webSearch=False)
                mode(legacy, "disabled", "disabled")
                legacy_on = new(app, "codex", webSearch=True)
                mode(legacy_on, "auto", "cached")
                passed("typed mode validation is side-effect free and legacy booleans map to disabled or preferred Auto")

                for preferred in ["cached", "live", "disabled"]:
                    mode(set_mode(app, chat, preferred), preferred, preferred)
                    if preferred == "disabled":
                        answer(app, chat, "chat", "disabled")
                    else:
                        plan = provider.plan("chat", preferred, kind="final")
                        turn_id = start_turn(app, chat); provider.request(); completed(app, chat, turn_id)
                        value = thread(app, chat)
                        verify_sources(value, plan)
                        # This thread has previous independent answers. Match by providerId.
                        cited_answer(value, plan)
                    mode(thread(app, chat), preferred, preferred)
                passed("cached/live/disabled reach the real provider unchanged and final-only results persist sources")

                stream_thread = new(app, "chat", "cached")
                plan = provider.plan("chat", "cached", kind="stream")
                turn_id = start_turn(app, stream_thread["id"]); provider.request(); completed(app, stream_thread["id"], turn_id)
                value = thread(app, stream_thread["id"])
                call = verify_sources(value, plan); result = cited_answer(value, plan)
                progress = item_events(app, stream_thread["id"], turn_id, "webSearch")
                require(any(item.get("status") in ["in_progress", "searching"] for item in progress), "real SSE progress never reached standard timeline notifications")
                matching_calls = [item for item in progress if item.get("providerId") == plan.provider_id]
                require(len({item["id"] for item in matching_calls}) == 1, "one search call changed its UI identity across live/final snapshots")
                messages = item_events(app, stream_thread["id"], turn_id, "agentMessage")
                require(messages and len({item["id"] for item in messages}) == 1 and messages[-1]["citations"], "richer final answer became a second item or lost its last annotations")
                require(len({item["url"] for item in call["sources"]}) == 3 and {item["url"] for item in result["citations"]} == {SOURCE}, "Sources was synthesized from citation-only URLs")
                persisted_thread = stream_thread["id"]
                passed("split UTF8 SSE, provider-unit multi-part citations and full Sources use stable item identities")

                untrusted = new(app, "chat", "cached")
                poison = provider.plan("chat", "cached", kind="private-marker")
                turn_id = start_turn(app, untrusted["id"]); provider.request(); completed(app, untrusted["id"], turn_id)
                value = thread(app, untrusted["id"])
                verify_sources(value, poison); cited_answer(value, poison)
                messages = item_events(app, untrusted["id"], turn_id, "agentMessage")
                require(messages and messages[-1].get("citations") and messages[-1]["text"] == "".join(answer_texts(poison)), "provider metadata spoofed activation and suppressed the completed answer")
                require("forged-provider" not in json.dumps(value), "provider control fields entered public history")
                for path in store.rglob("*.jsonl"):
                    require("forged-provider" not in path.read_text(encoding="utf-8"), "provider control fields were persisted as trusted raw history")
                passed("untrusted provider control fields cannot replace UI identities, hide final answers or enter trusted history")

                actions = new(app, "work", "live")
                plan = provider.plan("work", "live", kind="actions")
                turn_id = start_turn(app, actions["id"]); provider.request(); completed(app, actions["id"], turn_id)
                calls = search_items(thread(app, actions["id"]))
                require(len(calls) == 3 and {item["actionType"] for item in calls} == {"search", "open_page", "find_in_page"}, "real search/open/find actions were merged, omitted or guessed")
                require(next(item for item in calls if item["actionType"] == "find_in_page")["pattern"] == "中文", "find-in-page lost its exact pattern")
                require(all(item["url"] == SOURCE for item in calls if item["actionType"] != "search"), "open/find action lost its target")
                passed("search/open-page/find-in-page activities remain distinct and preserve action metadata")

                overrides = new(app, "chat", "disabled")
                answer(app, overrides["id"], "chat", "cached", webSearchMode="cached")
                mode(thread(app, overrides["id"]), "cached", "cached")
                answer(app, overrides["id"], "chat", "disabled", webSearch=False)
                mode(thread(app, overrides["id"]), "disabled", "disabled")
                answer(app, overrides["id"], "chat", "auto", webSearch=True)
                mode(thread(app, overrides["id"]), "auto", "auto")
                passed("per-turn typed and legacy overrides are sticky for subsequent turns")

                required = new(app, "work", "required")
                mode(required, "required", "required")
                first = provider.plan("work", "required", kind="loop", choice="required")
                provider.plan("work", "required", choice="auto", kind="reference", text="已运行批准的本机 true，并参考先前来源。")
                turn_id = start_turn(app, required["id"]); provider.request()
                approval = app.approval(required["id"])
                app.respond_approval(approval["id"], "accept")
                followup = provider.request()["body"]
                require(any(item.get("type") == "function_call_output" for item in followup["input"]), "approved real shell result did not reach the next Responses loop")
                completed(app, required["id"], turn_id)
                verify_sources(thread(app, required["id"]), first)
                mode(thread(app, required["id"]), "required", "required")
                passed("manual required search forces only the first request and later real local-tool loops use Auto")

                rpc(app, "desktop/settings/update", {"preferences": {"webSearchAllowed": False}})
                cap = rpc(app, "web/search/capabilities")
                require(cap["available"] is True and cap["policy"]["allowed"] is False, "global policy confused account connection with search authorization")
                mode(set_mode(app, chat, "live"), "live", "disabled")
                answer(app, chat, "chat", "disabled")
                for app_mode in ["work", "codex"]:
                    target = defaults[app_mode]["id"]
                    mode(set_mode(app, target, "cached"), "cached", "disabled")
                    answer(app, target, app_mode, "disabled")
                count_before = len(provider.history)
                threads_before = {item["id"] for item in rpc(app, "thread/list")["data"]}
                rejected(app, "thread/webSearch/set", {"threadId": chat, "mode": "required"})
                rejected(app, "thread/start", {"mode": "chat", "webSearchMode": "required"})
                rejected(app, "turn/start", {"threadId": chat, "webSearchMode": "required", "input": [{"type": "text", "text": "must not force blocked search"}]})
                require(len(provider.history) == count_before, "global search prohibition still allowed a required request")
                require({item["id"] for item in rpc(app, "thread/list")["data"]} == threads_before, "blocked required search left an orphan chat")
                mode(thread(app, chat), "live", "disabled")
                rpc(app, "desktop/settings/update", {"preferences": {"webSearchAllowed": True}})
                mode(thread(app, chat), "live", "live")
                mode(thread(app, defaults["codex"]["id"]), "cached", "cached")
                passed("global prohibition gates all modes, rejects forced search and retains each stored preference")

                off = new(app, "chat", "disabled")
                for kind in ["added", "done", "progress", "final"]:
                    plan = provider.plan("chat", "disabled", kind="unauthorized-" + kind)
                    turn_id = start_turn(app, off["id"]); provider.request(); completed(app, off["id"], turn_id, "failed")
                    no_new_search(app, off["id"], turn_id)
                    require(not any(item.get("providerId") == plan.provider_id for item in search_items(thread(app, off["id"]))), "an undeclared provider search was persisted as authorized work")
                mode(set_mode(app, persisted_thread, "disabled"), "disabled", "disabled")
                turn_id, _, _ = answer(app, persisted_thread, "chat", "disabled")
                # Legitimate citations may refer to prior sources even when no new search is allowed.
                provider.plan("chat", "disabled", kind="reference", text="仅引用先前已记录的资料。")
                turn_id = start_turn(app, persisted_thread); provider.request(); completed(app, persisted_thread, turn_id)
                no_new_search(app, persisted_thread, turn_id)
                value = thread(app, persisted_thread)
                require(any(item.get("type") == "agentMessage" and item.get("text") == "仅引用先前已记录的资料。" and item.get("citations", [{}])[0].get("url") == SOURCE for item in all_items(value)), "turning search off discarded legitimate citation metadata from an ordinary answer")
                passed("disabled search rejects all unsolicited hosted-call event forms without inventing activity; old citations remain usable")

                stopped_thread = new(app, "chat", "cached")
                plan = provider.plan("chat", "cached", kind="slow")
                turn_id = start_turn(app, stopped_thread["id"]); provider.request()
                require(plan.opened.wait(timeout), "slow search fixture did not begin")
                app.pump_until(lambda: bool(item_events(app, stopped_thread["id"], turn_id, "webSearch")), "live search progress before Stop")
                mode(set_mode(app, stopped_thread["id"], "disabled"), "disabled", "disabled")
                require(not any(item.get("method") == "turn/completed" and item.get("params", {}).get("threadId") == stopped_thread["id"] and item["params"].get("turn", {}).get("id") == turn_id for item in app.notifications), "changing preference interrupted the already-running request")
                rpc(app, "turn/interrupt", {"threadId": stopped_thread["id"], "turnId": turn_id})
                completed(app, stopped_thread["id"], turn_id, "interrupted")
                count_before = len(provider.history)
                plan.release.set(); require(plan.finished.wait(timeout), "stopped fixture did not release its late provider result")
                value = thread(app, stopped_thread["id"])
                require(not any(item.get("providerId") == plan.provider_id and item.get("status") == "completed" for item in search_items(value)), "Stop converted partial search progress into a completed search")
                require(not any(item.get("providerId") == plan.message_id or item.get("text") == "".join(answer_texts(plan)) for item in all_items(value)), "late stopped answer was persisted")
                require(len(provider.history) == count_before, "Stop caused an unrequested search retry")
                answer(app, stopped_thread["id"], "chat", "disabled")
                passed("preference changes affect only the next turn; Stop rejects late results and the next disabled turn resumes normally")

                failures = new(app, "chat", "auto")
                plan = provider.plan("chat", "auto", kind="failed")
                turn_id = start_turn(app, failures["id"]); provider.request()
                require(plan.opened.wait(timeout), "failed search fixture did not begin")
                plan.release.set(); completed(app, failures["id"], turn_id, "failed")
                value = thread(app, failures["id"])
                require(not any(item.get("providerId") == plan.provider_id and item.get("status") == "completed" for item in search_items(value)), "failed API stream fabricated a completed search result")
                provider.plan("chat", "auto", kind="invalid-url")
                turn_id = start_turn(app, failures["id"]); provider.request(); completed(app, failures["id"], turn_id, "failed")
                require("javascript:alert" not in json.dumps(all_items(thread(app, failures["id"]))), "unsafe provider navigation target entered public search history")
                passed("search stream failures and unsafe source targets stay failed and never become successful source rows")

                # Use a clean one-answer chat for unambiguous durable fork/replay checks.
                durable = new(app, "chat", "cached")
                plan = provider.plan("chat", "cached", kind="final")
                turn_id = start_turn(app, durable["id"]); provider.request(); completed(app, durable["id"], turn_id)
                durable_id, durable_plan = durable["id"], plan
                forked = rpc(app, "thread/fork", {"threadId": durable_id})["thread"]
                mode(forked, "cached", "cached")
                verify_sources(thread(app, forked["id"]), plan); cited_answer(thread(app, forked["id"]), plan)
                mode(set_mode(app, forked["id"], "disabled"), "disabled", "disabled")
                _, _, replay = answer(app, forked["id"], "chat", "disabled")
                historical = replay["body"]["input"]
                require(any(item.get("type") == "web_search_call" and item.get("id") == plan.provider_id for item in historical), "fork replay lost raw completed hosted-search context")
                source_message = next((item for item in historical if item.get("type") == "message" and item.get("id") == plan.message_id), None)
                require(source_message and source_message["content"][0]["annotations"][0]["start_index"] == 2, "fork replay lost original provider citation annotations")
                stop(app)
                app = start()
                resumed = rpc(app, "thread/resume", {"threadId": durable_id})["thread"]
                mode(resumed, "cached", "cached")
                verify_sources(thread(app, durable_id), durable_plan); cited_answer(thread(app, durable_id), durable_plan)
                mode(rpc(app, "thread/resume", {"threadId": forked["id"]})["thread"], "disabled", "disabled")
                require(rpc(app, "web/search/capabilities")["policy"]["allowed"] is True, "global preference did not survive process restart")
                passed("search preferences, complete consulted sources and original annotations survive fork, model replay and process resume")

                for configured in ["disabled", "live", "cached"]:
                    path = root / ("config-" + configured); path.mkdir()
                    (path / "config.toml").write_text(f'model = "{MODEL}"\nweb_search = "{configured}"\n', encoding="utf-8")
                    configured_app = start(path)
                    cap = rpc(configured_app, "web/search/capabilities", {"sandboxMode": "danger-full-access"})
                    require(cap["defaultModes"]["codex"] == configured, "Codex config did not override its full-access default")
                    created = new(configured_app, "codex")
                    mode(created, "auto", configured)
                    answer(configured_app, created["id"], "codex", configured)
                    stop(configured_app)
                passed("Codex preferred Auto resolves explicit cached/live/disabled config ahead of sandbox defaults")

                oauth_store = root / "oauth"; oauth_store.mkdir()
                (oauth_store / "auth.json").write_text(json.dumps({"auth_mode": "chatgpt", "tokens": {"access_token": ACCESS, "refresh_token": REFRESH, "account_id": "fixture-search-account"}}), encoding="utf-8")
                (oauth_store / "auth.json").chmod(0o600)
                oauth = start(oauth_store, credential=None)
                cap = rpc(oauth, "web/search/capabilities")
                require(cap["available"] is True and cap["permissionsVerified"] is False, "OAuth search was either blocked without evidence or advertised as entitled")
                created = new(oauth, "chat")
                plan = provider.plan("chat", "auto", kind="final", key=ACCESS, account_id="fixture-search-account")
                turn_id = start_turn(oauth, created["id"]); provider.request(); completed(oauth, created["id"], turn_id)
                verify_sources(thread(oauth, created["id"]), plan); cited_answer(thread(oauth, created["id"]), plan)
                stop(oauth)
                passed("unexpired OAuth fixture uses the configured hosted endpoint with account headers and unknown entitlement")

                multiplex = new(app, "chat", "cached")
                multi = provider.plan("chat", "cached", kind="multi")
                turn_id = start_turn(app, multiplex["id"]); provider.request()
                require(multi.opened.wait(timeout), "multi-message fixture did not reach its live last-delta boundary")
                def last_delta_observed() -> bool:
                    return any(event.get("method") == "item/agentMessage/delta" and event.get("params", {}).get("threadId") == multiplex["id"] and event["params"].get("turnId") == turn_id and event["params"].get("delta") == f"｜最后B（{multi.number}）" for event in app.notifications)
                app.pump_until(last_delta_observed, "anonymous message's active final Unicode delta")
                active = message_timeline(app, multiplex["id"], turn_id)
                anonymous = [state for state in active.values() if state["text"] == multi_texts(multi)[1]]
                require(len(anonymous) == 1 and anonymous[0]["closed"] is False and not multi.release.is_set(), "the active anonymous answer was ended before the provider completed it")
                multi.release.set(); completed(app, multiplex["id"], turn_id)
                received = message_timeline(app, multiplex["id"], turn_id)
                require(len(received) == 4 and sorted(state["text"] for state in received.values()) == sorted(multi_texts(multi)), "wire timeline lost or duplicated an interleaved provider message")
                for text in multi_texts(multi)[:2]:
                    state = next(state for state in received.values() if state["text"] == text)
                    require(state["closed"] and len(state["item"].get("citations", [])) == 1, "done-to-final citation update created another item or failed to enrich its original item")
                verify_multi(thread(app, multiplex["id"]), multi)
                eof_stop(app); app = start()
                rpc(app, "thread/resume", {"threadId": multiplex["id"]})
                verify_multi(thread(app, multiplex["id"]), multi)
                passed("named/anonymous multi-message Unicode deltas survive reasoning/search interleaving, richer final metadata and restart without text deduplication")

                for outcome in ["interrupted", "failed"]:
                    partial = new(app, "chat", "cached")
                    plan = provider.plan("chat", "cached", kind="partial-" + outcome)
                    turn_id = start_turn(app, partial["id"]); provider.request()
                    require(plan.opened.wait(timeout), "partial persistence fixture did not stream")
                    def partial_observed() -> bool:
                        return any(event.get("method") == "item/agentMessage/delta" and event.get("params", {}).get("threadId") == partial["id"] and event["params"].get("turnId") == turn_id and event["params"].get("delta") == "｜最后一段仍在生成" for event in app.notifications)
                    app.pump_until(partial_observed, "observed live partial answer before termination")
                    live_messages = message_timeline(app, partial["id"], turn_id)
                    require(len(live_messages) == 1 and next(iter(live_messages.values()))["text"] == partial_text(plan), "live partial answer lost its final active delta")
                    if outcome == "interrupted":
                        rpc(app, "turn/interrupt", {"threadId": partial["id"], "turnId": turn_id})
                        completed(app, partial["id"], turn_id, "interrupted")
                        plan.release.set()
                    else:
                        plan.release.set(); completed(app, partial["id"], turn_id, "failed")
                    require(plan.finished.wait(timeout), "terminated partial fixture did not finish")
                    verify_partial(thread(app, partial["id"]), plan, outcome)
                    eof_stop(app); app = start()
                    resumed = rpc(app, "thread/resume", {"threadId": partial["id"]})["thread"]
                    mode(resumed, "cached", "cached")
                    verify_partial(thread(app, partial["id"]), plan, outcome)
                    forked = rpc(app, "thread/fork", {"threadId": partial["id"]})["thread"]
                    verify_partial(thread(app, forked["id"]), plan, outcome)
                    for target, effective in [(partial["id"], "cached"), (forked["id"], "disabled")]:
                        mode(set_mode(app, target, effective), effective, effective)
                        saved_ids = {item["id"] for item in all_items(thread(app, target))}
                        if effective == "cached":
                            provider.plan("chat", "cached", kind="final")
                            followup = start_turn(app, target)
                            wire = provider.request()
                            completed(app, target, followup)
                        else:
                            followup, _, wire = answer(app, target, "chat", "disabled")
                        new_ids = {item["id"] for item in item_events(app, target, followup)}
                        require(not saved_ids.intersection(new_ids), "new live item reused a saved partial UI identity after app-server restart")
                        verify_model_has_no_ui_snapshot(wire["body"], plan)
                        verify_partial(thread(app, target), plan, outcome)
                passed("stopped/failed observed UI survives EOF, process restart, resume and fork while real model replay excludes private snapshots and synthetic search success")

                require(not provider.errors and provider.plans.empty() and provider.requests.empty(), "unresolved mock plans, model requests or provider protocol failures remain: " + "; ".join(provider.errors))
                for instance in live:
                    public(instance.notifications)
                for path in store.rglob("*.json"):
                    if path.name != "auth.json":
                        public(path.read_text(encoding="utf-8"))
            finally:
                for app in list(live):
                    stop(app)
            print(f"PASS search smoke: {checks} checks, {rpc_count} RPCs, {len(provider.history)} local Responses HTTP requests; real Rust sessions and hosted tool declarations; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=8)
    options = parser.parse_args()
    require(options.binary.is_file(), "build mcodex before running the search smoke")
    require(0 < options.timeout <= 10, "timeout must be at most 10 seconds")
    try:
        run(options.binary.resolve(), options.timeout)
        return 0
    except Exception as error:
        message = str(error)
        for secret in SECRETS:
            message = message.replace(secret, "[redacted fixture credential]")
        print("FAIL search smoke: " + message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
