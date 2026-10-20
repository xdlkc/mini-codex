#!/usr/bin/env python3
"""Verify persistent attachments through real JSONL RPCs and loopback model turns.

Build first with `cargo build`, then run `python3 scripts/smoke-files.py`.
Only Python's standard library is required. Credentials are fixed test strings;
HOME, sessions, imported files, and workspace all live in a disposable directory.
"""

from __future__ import annotations

import argparse
import base64
from collections import deque
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import re
import stat
import struct
import subprocess
import sys
import tempfile
import threading
from typing import Any
import zipfile
import zlib

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("mcodex_files_turn_helpers", Path(__file__).with_name("smoke-turns.py"))
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
require, SmokeFailure, MockResponses = HELPERS.require, HELPERS.SmokeFailure, HELPERS.MockResponses


class FileServer(HELPERS.AppServer):
    """Use the shared bounded RPC/event pump with an allowlisted child env."""

    def __init__(self, binary: Path, store: Path, workspace: Path, fixture_home: Path, base_url: str, timeout: float):
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(fixture_home),
            "CODEX_HOME": str(fixture_home / "codex"),
            "MCODEX_HOME": str(store),
            "MCODEX_NO_CODEX_IMPORT": "1",
            "OPENAI_API_KEY": "local-test-only",
            "OPENAI_BASE_URL": base_url,
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
            "LANG": "en_US.UTF-8",
            "TMPDIR": str(fixture_home),
        }
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

    def reject(self, method: str, params: dict[str, Any], contains: str = "") -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": request_id, "method": method, "params": params}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        self.rpc_count += 1
        self.pump_until(lambda: request_id in self.replies, method)
        response = self.replies.pop(request_id)
        require("error" in response, f"{method} unexpectedly succeeded")
        error = response["error"]
        require(contains.lower() in error.get("message", "").lower(), f"{method} failed for the wrong reason: {error}")
        return error

    def initialize_isolated(self, store: Path) -> None:
        result = self.rpc("initialize")
        require(result.get("codexImportDisabled") is True and Path(result["codexHome"]) == store, "binary did not confirm isolated attachment state; rebuild first")


def encoded(data: bytes) -> str:
    return base64.b64encode(data).decode("ascii")


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def assert_no_store_path(value: Any, store: Path) -> None:
    require(str(store) not in json.dumps(value, ensure_ascii=False), "public attachment metadata/error leaked the absolute store path")


def assert_public_model_request(body: dict[str, Any]) -> None:
    def visit(value: Any) -> None:
        if isinstance(value, dict):
            require(not any(key.startswith("_mcodex_") or key in {"attachmentId", "attachments"} for key in value), "private attachment presentation fields reached the model")
            if value.get("type") == "input_text":
                require(set(value) <= {"type", "text"}, "input_text contains attachment metadata fields")
            if value.get("type") == "input_image":
                require(set(value) <= {"type", "image_url", "detail"}, "input_image contains attachment metadata fields")
            for child in value.values():
                visit(child)
        elif isinstance(value, list):
            for child in value:
                visit(child)
    visit(body)


def user_items(thread: dict[str, Any]) -> list[dict[str, Any]]:
    return [item for turn in thread.get("turns", []) for item in turn.get("items", []) if item.get("type") == "userMessage"]


def assert_presentation(item: dict[str, Any], prompt: str, expected: list[dict[str, Any]], store: Path) -> None:
    require("".join(part.get("text", "") for part in item.get("content", [])) == prompt, "visible user body contains extracted attachment content or lost the human prompt")
    cards = [part for part in item.get("attachments", []) if "id" in part]
    require({part["id"] for part in cards} == {part["id"] for part in expected} and len(cards) == len(expected), "restored attachment card identities differ from imported files")
    for card in cards:
        original = next(part for part in expected if part["id"] == card["id"])
        require(card["name"] == original["name"] and card["sha256"] == original["sha256"] and card["parseStatus"] == "ready", "attachment card metadata changed during live/history restoration")
        require("text" not in card and "dataBase64" not in card and "path" not in card, "attachment card embeds private content or a storage path")
        assert_no_store_path(card, store)


def package(parts: dict[str, str], overrides: dict[str, str]) -> bytes:
    stream = io.BytesIO()
    types = '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/>'
    types += "".join(f'<Override PartName="/{name}" ContentType="{mime}"/>' for name, mime in overrides.items()) + "</Types>"
    with zipfile.ZipFile(stream, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", types)
        for name, content in parts.items():
            archive.writestr(name, content.encode("utf-8"))
    return stream.getvalue()


def pdf(text: str | None) -> bytes:
    content = b"" if text is None else b"BT /F1 12 Tf 30 50 Td (" + text.encode("ascii").replace(b"\\", b"\\\\").replace(b"(", b"\\(").replace(b")", b"\\)") + b") Tj ET\n"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        f"<< /Length {len(content)} >>\nstream\n".encode() + content + b"\nendstream",
    ]
    output = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0]
    for index, obj in enumerate(objects, 1):
        offsets.append(len(output))
        output.extend(f"{index} 0 obj\n".encode() + obj + b"\nendobj\n")
    start = len(output)
    output.extend(f"xref\n0 {len(objects)+1}\n0000000000 65535 f \n".encode())
    output.extend(b"".join(f"{offset:010d} 00000 n \n".encode() for offset in offsets[1:]))
    output.extend(f"trailer\n<< /Size {len(objects)+1} /Root 1 0 R >>\nstartxref\n{start}\n%%EOF\n".encode())
    return bytes(output)


def png() -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"\0\x10\x20\x30\xff\x40\x50\x60\xff" * 2)) + chunk(b"IEND", b"")


def fixtures() -> dict[str, tuple[str, bytes]]:
    csv = 'index,"中文, value",amount\r\n' + "".join(f'{i},"引用,中文 {i}",{i}.25\r\n' for i in range(10_000)) + "END_FULL_CSV_尾部,complete,42\r\n"
    require(len(csv.encode()) > 200_000, "CSV fixture does not exceed the old inline-text limit")
    docx = package({
        "_rels/.rels": '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rDoc" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>',
        "word/document.xml": '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>DOCX 中文 &amp; &#x1F600;</w:t><w:tab/><w:t>COMPLETE_DOCX_END</w:t></w:r></w:p></w:body></w:document>',
        "word/header1.xml": '<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>HEADER_INCLUDED</w:t></w:r></w:p></w:hdr>',
    }, {"word/document.xml": "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"})
    pptx = package({
        "ppt/presentation.xml": '<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst><p:sldId id="257" r:id="second"/><p:sldId id="256" r:id="first"/></p:sldIdLst></p:presentation>',
        "ppt/_rels/presentation.xml.rels": '<Relationships><Relationship Id="first" Target="slides/slide1.xml"/><Relationship Id="second" Target="slides/slide2.xml"/></Relationships>',
        "ppt/slides/slide1.xml": '<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><a:p><a:r><a:t>FIRST_SLIDE</a:t></a:r></a:p></p:cSld></p:sld>',
        "ppt/slides/slide2.xml": '<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><a:p><a:r><a:t>SECOND_SLIDE 中文</a:t></a:r></a:p></p:cSld></p:sld>',
        "ppt/slides/_rels/slide2.xml.rels": '<Relationships><Relationship Id="notes" Target="../notesSlides/notesSlide2.xml"/></Relationships>',
        "ppt/notesSlides/notesSlide2.xml": '<p:notes xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>SPEAKER_NOTES_INCLUDED</a:t></a:r></a:p></p:notes>',
    }, {"ppt/presentation.xml": "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"})
    xlsx = package({
        "xl/workbook.xml": '<workbook xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="账目 &amp; data" sheetId="1" r:id="sheet1"/></sheets></workbook>',
        "xl/_rels/workbook.xml.rels": '<Relationships><Relationship Id="sheet1" Target="worksheets/sheet1.xml"/></Relationships>',
        "xl/sharedStrings.xml": '<sst><si><r><t>shared </t></r><r><t>中文</t></r></si></sst>',
        "xl/worksheets/sheet1.xml": '<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="inlineStr"><is><t>inline &amp; value</t></is></c><c r="C1" t="b"><v>1</v></c><c r="D1"><f>SUM(A2:A3)</f><v>42.50</v></c><c r="E1"><f t="shared" si="0"/><v>3</v></c></row></sheetData></worksheet>',
    }, {"xl/workbook.xml": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"})
    return {
        "plain.txt": ("text/plain", "全文 中文 😀\r\nsecond line\n".encode()),
        "records.csv": ("text/csv", csv.encode()),
        "data.json": ("application/json", json.dumps({"name": "中文", "values": [1, 2, 3], "safe": "<script>data</script>"}, ensure_ascii=False, indent=2).encode()),
        "notes.md": ("text/markdown", "# 标题\n\n**完整 Markdown**\nEND_MD\n".encode()),
        "literal.html": ("text/html", b"<script>throw Error('MUST_NOT_EXECUTE')</script><p>literal source</p>"),
        "wide.txt": ("text/plain", b"\xff\xfe" + "UTF16 中文 😀\n".encode("utf-16-le")),
        "report.pdf": ("application/pdf", pdf("REAL_PDF_TEXT_COMPLETE")),
        "report.docx": ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", docx),
        "deck.pptx": ("application/vnd.openxmlformats-officedocument.presentationml.presentation", pptx),
        "book.xlsx": ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", xlsx),
        "image.png": ("image/png", png()),
    }


def run(binary: Path, timeout: float) -> None:
    checks = requests = rpc_count = 0
    with tempfile.TemporaryDirectory(prefix="mcodex-files-smoke-") as directory:
        root = Path(directory).resolve()
        store, workspace, fixture_home = root / "mcodex", root / "workspace", root / "home"
        for path in [store, workspace, fixture_home]:
            path.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        data = fixtures()
        saved: dict[str, dict[str, Any]] = {}
        texts: dict[str, str] = {}
        prompt = "请核对附件。可见正文只保留这句人工提示。"
        attached_names = ["records.csv", "report.pdf", "report.docx", "deck.pptx", "book.xlsx", "literal.html", "image.png"]
        with MockResponses(timeout) as mock:
            with FileServer(binary, store, workspace, fixture_home, mock.base_url, timeout) as app:
                app.initialize_isolated(store)
                for name, (mime, raw) in data.items():
                    metadata = app.rpc("files/import", {"name": name, "mimeType": mime, "dataBase64": encoded(raw)})["attachment"]
                    require(metadata["name"] == name and metadata["size"] == len(raw) and metadata["sha256"] == digest(raw) and metadata["parseStatus"] == "ready", f"real fixture did not import intact: {name}: {metadata}")
                    require(re.fullmatch(r"att_[0-9a-f]{32}", metadata["id"]) is not None, "attachment id is not an opaque persistent id")
                    assert_no_store_path(metadata, store)
                    require("content" not in app.rpc("files/read", {"id": metadata["id"]}), "metadata-only read returned content")
                    read = app.rpc("files/read", {"id": metadata["id"], "includeContent": True, "includeOriginal": True})
                    require(base64.b64decode(read["content"]["dataBase64"], validate=True) == raw, f"exported original changed bytes: {name}")
                    if metadata["kind"] != "image":
                        texts[name] = read["content"]["text"]
                        require(metadata["textBytes"] == len(texts[name].encode()) and metadata["textSha256"] == digest(texts[name].encode()), f"extracted text hash/size mismatch: {name}")
                    folder = store / "attachments" / metadata["id"]
                    require((folder / "original.bin").read_bytes() == raw, "private original differs from imported bytes")
                    if os.name == "posix":
                        require(stat.S_IMODE(folder.stat().st_mode) == 0o700, "attachment directory is not private")
                        for path in folder.iterdir():
                            require(path.is_file() and stat.S_IMODE(path.stat().st_mode) == 0o600, "attachment content/metadata permissions are not private")
                    saved[name] = metadata
                for name in ["plain.txt", "records.csv", "data.json", "notes.md", "literal.html"]:
                    require(texts[name] == data[name][1].decode(), f"plain text source was changed: {name}")
                require(texts["wide.txt"] == "UTF16 中文 😀\n", "UTF-16 source was decoded incorrectly")
                require("REAL_PDF_TEXT_COMPLETE" in texts["report.pdf"], "PDF text is missing")
                require("DOCX 中文 & 😀\tCOMPLETE_DOCX_END" in texts["report.docx"] and "HEADER_INCLUDED" in texts["report.docx"], "DOCX entities/header text are missing")
                require(texts["deck.pptx"].find("SECOND_SLIDE") < texts["deck.pptx"].find("FIRST_SLIDE") and "SPEAKER_NOTES_INCLUDED" in texts["deck.pptx"], "PowerPoint order or speaker notes were lost")
                require(all(value in texts["book.xlsx"] for value in ["账目 & data", "shared 中文", "inline & value", "true", "SUM(A2:A3)", "42.50", "attributes=", "cachedValue="]), "worksheet values, formulas, or cached data are missing")
                listed = app.rpc("files/list")["data"]
                require({item["id"] for item in listed} == {item["id"] for item in saved.values()}, "list did not return all independently imported files")
                checks += 1
                print("PASS import/read/list: 11 real formats/encodings, full text, original bytes/hash, private permissions", flush=True)

                before = {item["id"] for item in listed}
                invalid = [
                    ("files/import", {"name": "../escape.txt", "dataBase64": encoded(b"safe")}, "filename"),
                    ("files/import", {"name": "bad.txt", "dataBase64": "not base64"}, "base64"),
                    ("files/import", {"name": "fake.png", "dataBase64": encoded(b"not an image")}, "image"),
                    ("files/read", {"id": "../../outside"}, "id"),
                    ("files/delete", {"id": "../../outside"}, "id"),
                    ("files/import", {"name": "large.txt", "dataBase64": encoded(b"x" * (8 * 1024 * 1024 + 1))}, "limit exceeded"),
                ]
                bomb = package({"word/document.xml": "x" * (16 * 1024 * 1024 + 1)}, {})
                invalid.append(("files/import", {"name": "bomb.docx", "dataBase64": encoded(bomb)}, "ZIP entry"))
                for method, params, reason in invalid:
                    assert_no_store_path(app.reject(method, params, reason), store)
                require({item["id"] for item in app.rpc("files/list")["data"]} == before, "rejected input created a partial attachment")
                require(not any(path.name.startswith(".import-") for path in (store / "attachments").iterdir()), "failed import left a partial directory")
                checks += 1
                print("PASS rejected paths/base64/images/text and ZIP limits leave no partial records or path leaks", flush=True)

                thread_id = app.rpc("thread/start", {"mode": "chat"})["thread"]["id"]
                cards = [saved[name] for name in attached_names]
                scenario = mock.plan("success", "你好，完整附件已收到 🙂")
                turn_id = app.turn(thread_id, [{"type": "text", "text": prompt}] + [{"type": "attachment", "attachmentId": item["id"]} for item in cards])
                body = mock.request()
                requests += 1
                assert_public_model_request(body)
                parts = body["input"][-1]["content"]
                model_text = [part["text"] for part in parts if part.get("type") == "input_text"]
                require(prompt in model_text, "human prompt did not reach the model")
                for name in attached_names:
                    if name != "image.png":
                        require(any(texts[name] in value for value in model_text), f"model did not receive full extracted content: {name}")
                require(any(part.get("type") == "input_image" and part.get("image_url") == "data:image/png;base64," + encoded(data["image.png"][1]) for part in parts), "model did not receive the original image data URL")
                app.assert_chinese_completion(thread_id, turn_id, scenario.text)
                live_users = [event["params"]["item"] for event in app.notifications if event.get("method") in {"item/started", "item/completed"} and event.get("params", {}).get("threadId") == thread_id and event["params"].get("item", {}).get("type") == "userMessage"]
                require(live_users, "live userMessage notification was missing")
                for item in live_users:
                    assert_presentation(item, prompt, cards, store)
                historical = app.rpc("thread/read", {"threadId": thread_id, "includeTurns": True})["thread"]
                require(len(user_items(historical)) == 1, "completed history duplicated the attachment message")
                assert_presentation(user_items(historical)[0], prompt, cards, store)
                checks += 1
                print("PASS attachmentId sends complete text/original image to model; live/history keep prompt and metadata cards", flush=True)

                slow_prompt = "后台运行时只显示此人工提示。"
                slow_cards = [saved["notes.md"]]
                slow = mock.plan("slow")
                slow_id = app.turn(thread_id, [{"type": "text", "text": slow_prompt}, {"type": "attachment", "attachmentId": slow_cards[0]["id"]}])
                slow_body = mock.request()
                requests += 1
                assert_public_model_request(slow_body)
                require(slow.opened.wait(timeout), "slow attachment stream never opened")
                app.event("item/agentMessage/delta", thread_id, slow_id)
                active = app.rpc("thread/resume", {"threadId": thread_id})["thread"]
                require(active["status"]["type"] == "active" and len(user_items(active)) == 2, "active resume lost or duplicated attachment messages")
                assert_presentation(user_items(active)[-1], slow_prompt, slow_cards, store)
                app.rpc("turn/interrupt", {"threadId": thread_id})
                require(app.event("turn/completed", thread_id, slow_id)["turn"]["status"] == "interrupted", "attachment stream failed to interrupt")
                slow.release.set()
                app.event("thread/status/changed", thread_id, status="idle")
                fork_id = app.rpc("thread/fork", {"threadId": thread_id})["thread"]["id"]
                forked = app.rpc("thread/read", {"threadId": fork_id, "includeTurns": True})["thread"]
                require(len(user_items(forked)) == 2, "fork dropped an attachment message")
                assert_presentation(user_items(forked)[0], prompt, cards, store)
                assert_presentation(user_items(forked)[1], slow_prompt, slow_cards, store)
                prefix = app.rpc("thread/fork", {"threadId": thread_id, "beforeUserMessage": 1})["thread"]
                require(len(user_items(prefix)) == 1, "fork visible message index does not preserve the attachment prefix")
                assert_presentation(user_items(prefix)[0], prompt, cards, store)
                checks += 1
                print("PASS active resume retains visible prompt/cards; interruption and full/prefix fork preserve attachment identity", flush=True)

                unread: list[dict[str, Any]] = []
                for name, raw, status in [("scan.pdf", pdf(None), "no_text"), ("broken.pdf", b"%PDF-1.4\ninvalid", "failed"), ("broken.docx", b"invalid zip", "failed")]:
                    metadata = app.rpc("files/import", {"name": name, "dataBase64": encoded(raw)})["attachment"]
                    require(metadata["parseStatus"] == status, "unreadable document was reported as ready")
                    assert_no_store_path(metadata, store)
                    rejected_thread = app.rpc("thread/start", {"mode": "chat"})["thread"]["id"]
                    marker = len(app.notifications)
                    error = app.reject("turn/start", {"threadId": rejected_thread, "input": [{"type": "text", "text": "Do not pretend this file was read"}, {"type": "attachment", "attachmentId": metadata["id"]}]})
                    assert_no_store_path(error, store)
                    require(not any(event.get("method") == "turn/started" and event.get("params", {}).get("threadId") == rejected_thread for event in app.notifications[marker:]), "unreadable attachment started a turn")
                    rejected_history = app.rpc("thread/read", {"threadId": rejected_thread, "includeTurns": True})["thread"]
                    require(not user_items(rejected_history), "rejected attachment recorded a false user/model turn")
                    require((store / "attachments" / metadata["id"] / "original.bin").read_bytes() == raw, "rejected turn deleted or changed the source attachment")
                    exported = app.rpc("files/read", {"id": metadata["id"], "includeOriginal": True})
                    require(base64.b64decode(exported["content"]["dataBase64"], validate=True) == raw, "unreadable original cannot be recovered")
                    unread.append(metadata)
                require(mock.requests.empty() and mock.plans.empty(), "unreadable attachment contacted the model")
                checks += 1
                print("PASS no_text/failed documents refuse turn start without model calls, false history, or lost originals", flush=True)
                delete_id = saved["plain.txt"]["id"]
                require(app.rpc("files/delete", {"id": delete_id}) == {"deleted": True, "id": delete_id}, "delete returned an unexpected result")
                assert_no_store_path(app.reject("files/read", {"id": delete_id}, "not found"), store)
                rpc_count += app.rpc_count

            with FileServer(binary, store, workspace, fixture_home, mock.base_url, timeout) as app:
                app.initialize_isolated(store)
                listed = app.rpc("files/list")["data"]
                require(delete_id not in {item["id"] for item in listed} and len(listed) == len(saved) + len(unread) - 1, "attachment deletion/list did not persist across restart")
                for metadata in cards:
                    read = app.rpc("files/read", {"id": metadata["id"], "includeContent": True, "includeOriginal": True})
                    raw = data[metadata["name"]][1]
                    require(read["attachment"] == metadata and base64.b64decode(read["content"]["dataBase64"], validate=True) == raw, "restarted attachment metadata/original differs")
                    if metadata["kind"] != "image":
                        require(read["content"]["text"] == texts[metadata["name"]], "restarted extracted content was lost")
                for restored_id in [thread_id, fork_id]:
                    restored = app.rpc("thread/resume", {"threadId": restored_id})["thread"]
                    require(len(user_items(restored)) == 2, "restarted session/fork lost attachment messages")
                    assert_presentation(user_items(restored)[0], prompt, cards, store)
                    assert_presentation(user_items(restored)[1], slow_prompt, slow_cards, store)
                scenario = mock.plan("success", "你好，重启后保留附件全文 🙂")
                followup = app.turn(fork_id, [{"type": "text", "text": "Continue from the saved attachment context."}])
                body = mock.request()
                requests += 1
                assert_public_model_request(body)
                historical_text = [part.get("text", "") for item in body["input"] if item.get("role") == "user" for part in item.get("content", []) if part.get("type") == "input_text"]
                require(any(texts["records.csv"] in text for text in historical_text) and any(texts["notes.md"] in text for text in historical_text), "fork/restart model history lost full attachment contents")
                require(any(part.get("image_url") == "data:image/png;base64," + encoded(data["image.png"][1]) for item in body["input"] if item.get("role") == "user" for part in item.get("content", [])), "fork/restart model history lost original image")
                app.assert_chinese_completion(fork_id, followup, scenario.text)
                require(mock.errors == [] and mock.requests.empty() and mock.plans.empty(), f"mock request/scenario mismatch: {mock.errors}")
                rpc_count += app.rpc_count
                checks += 1
                print("PASS restart preserves originals/text/cards/deletion; forked model history strips private UI fields", flush=True)
    print(f"PASS file smoke: {checks} groups, {rpc_count} JSONL RPCs, {requests} loopback model mocks; no external calls; temporary files removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target" / "debug" / "mcodex")
    parser.add_argument("--timeout", type=float, default=10, help="seconds per RPC/event/mock request (default: 10)")
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
        print(f"FAIL file smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
