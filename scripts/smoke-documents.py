#!/usr/bin/env python3
"""Exercise structured Pages through the real Rust JSONL app-server.

Build first with `cargo build`, then run `python3 scripts/smoke-documents.py`.
Two independent app-server processes share one disposable store. No model,
provider, real account, or user document is accessed. Only stdlib is required.
"""
from __future__ import annotations

import argparse
import base64
from collections import deque
import importlib.util
import io
import json
import os
from pathlib import Path
import queue
import stat
import struct
import subprocess
import sys
import tempfile
import threading
import zipfile
import zlib
import xml.etree.ElementTree as ET
from typing import Any

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("mcodex_document_helpers", Path(__file__).with_name("smoke-desktop.py"))
assert SPEC is not None and SPEC.loader is not None
HELPERS = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = HELPERS
SPEC.loader.exec_module(HELPERS)
require, SmokeFailure = HELPERS.require, HELPERS.SmokeFailure


class AppServer(HELPERS.AppServer):
    """Reuse the bounded JSONL pump with only explicitly isolated environment."""

    def __init__(self, binary: Path, store: Path, workspace: Path, fixture_home: Path, timeout: float):
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(fixture_home),
            "CODEX_HOME": str(fixture_home / "codex"),
            "MCODEX_HOME": str(store),
            "MCODEX_NO_CODEX_IMPORT": "1",
            "OPENAI_BASE_URL": "http://127.0.0.1:1",
            "NO_PROXY": "127.0.0.1,localhost",
            "LANG": "en_US.UTF-8",
            "TMPDIR": str(fixture_home),
        }
        self.timeout, self.next_id, self.rpc_count = timeout, 0, 0
        self.pending: dict[int, tuple[str, float]] = {}
        self.replies: dict[int, dict[str, Any]] = {}
        self.lines: queue.Queue[str | None] = queue.Queue()
        self.stderr: deque[str] = deque(maxlen=40)
        self.process = subprocess.Popen(
            [str(binary), "--model", "no-model-must-be-called", "-C", str(workspace), "app-server"],
            cwd=workspace, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1,
        )
        self.readers = [threading.Thread(target=self._read_stdout, daemon=True), threading.Thread(target=self._read_stderr, daemon=True)]
        for reader in self.readers:
            reader.start()


def body(text: str) -> dict[str, Any]:
    return {"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]}


def run(binary: Path, timeout: float) -> None:
    groups, servers = 0, []

    def passed(label: str) -> None:
        nonlocal groups
        groups += 1
        print(f"PASS {groups} {label}", flush=True)

    with tempfile.TemporaryDirectory(prefix="mcodex-document-smoke-") as temp:
        root = Path(temp)
        store, workspace, fixture_home = root / "store", root / "workspace", root / "os-home"
        workspace.mkdir()
        fixture_home.mkdir()

        def server() -> AppServer:
            instance = AppServer(binary, store, workspace, fixture_home, timeout)
            servers.append(instance)
            return instance

        try:
            with server() as a, server() as b:
                a.initialize(store)
                b.initialize(store)
                project = a.rpc("desktop/project/create", {"name": "Pages fixture", "mode": "work"})["project"]
                source = "  # Exact source\r\n<script>original</script>\0\n" + "中" * 600 + " FULL SEARCH NEEDLE"
                legacy = a.rpc("desktop/document/create", {"title": "Original", "content": source, "projectId": project["id"]})["document"]
                document_id, original = legacy["id"], body("A😀B")
                upgraded = a.rpc("desktop/document/update", {"id": document_id, "expectedRevision": 1, "format": "richtext", "body": original})["document"]
                require(upgraded["sourceMarkdown"] == source and upgraded["body"] == original and upgraded["revision"] == 2, "legacy upgrade lost its exact source or structured body")
                passed("legacy source and structured upgrade")

                saved = b.rpc("desktop/document/update", {"id": document_id, "expectedRevision": 2, "title": "Saved by second process"})["document"]
                require(saved["revision"] == 3, "second app-server did not load the latest revision")
                before = (store / "desktop.json").read_bytes()
                stale = a.rpc("desktop/document/update", {"id": document_id, "expectedRevision": 2, "body": body("stale")})
                require(stale.get("conflict") == {"expectedRevision": 2, "actualRevision": 3}, "stale write did not return a real CAS conflict")
                require(before == (store / "desktop.json").read_bytes(), "CAS conflict changed saved bytes")
                passed("two app-servers use one revision-checked store")

                comment = a.rpc("desktop/document/comments/create", {"id": document_id, "expectedRevision": 3, "body": "Local review", "anchor": {"revision": 3, "from": 1, "to": 5, "quote": "A😀B"}})["comment"]
                comments = b.rpc("desktop/document/comments/list", {"id": document_id})
                require(comments["revision"] == 4 and comments["items"][0]["orphaned"] is False, "comment mutation invalidated an unchanged UTF-16 anchor")
                changed = b.rpc("desktop/document/update", {"id": document_id, "expectedRevision": 4, "body": body("Changed")})
                require(changed["document"]["revision"] == 5, "body save did not advance revision")
                require(a.rpc("desktop/document/comments/list", {"id": document_id})["items"][0]["orphaned"] is True, "body change silently reused an old comment location")
                restored = a.rpc("desktop/document/restore", {"id": document_id, "expectedRevision": 5, "revision": 2})["document"]
                require(restored["revision"] == 6 and restored["body"] == original and restored["commentCount"] == 1, "restore lost the body or rewound current comments")
                passed("UTF-16 comment anchor, orphaning and history restore")

                exported = a.rpc("desktop/document/export", {"id": document_id, "format": "json", "revision": 2})
                exported_json = json.loads(exported["content"])
                require(exported_json["version"]["body"] == original and exported_json["sourceMarkdown"] == source, "JSON export is not a full restorable document")
                old = a.rpc("desktop/document/history/read", {"id": document_id, "revision": 1})
                require(old["version"]["content"] == source, "history lost its original Markdown bytes")
                history = a.rpc("desktop/document/history/list", {"id": document_id})
                require(history["history"]["count"] == 6 and len(history["items"]) == 6 and "body" not in history["items"][0], "history list returned the wrong revision count or full payload")
                summary = a.rpc("desktop/state/read")["documents"][0]
                require(all(key not in summary for key in ["body", "sourceMarkdown", "_history", "_comments"]), "state summaries contain full private document payloads")
                if os.name == "posix":
                    require(all(stat.S_IMODE((store / name).stat().st_mode) == 0o600 for name in ["desktop.json", "desktop.lock"]), "document store or lock is not private")
                passed("full JSON export, compact history and private persistence")

                # Exercise the public binary export, an independent ZIP/XML
                # reader, the real attachment parser and macOS's DOCX reader.
                def png_chunk(kind: bytes, value: bytes) -> bytes:
                    return struct.pack(">I", len(value)) + kind + value + struct.pack(">I", zlib.crc32(kind + value))
                png = b"\x89PNG\r\n\x1a\n" + png_chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 6, 0, 0, 0)) + png_chunk(b"IDAT", zlib.compress(b"\0\0\0\0\xff")) + png_chunk(b"IEND", b"")
                image = a.rpc("files/import", {"name": "pixel.png", "dataBase64": base64.b64encode(png).decode()})["attachment"]
                require(image["parseStatus"] == "ready", "Word image fixture is not a readable PNG")
                rich = {"type": "doc", "content": [
                    {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Word 中文 😀"}]},
                    {"type": "paragraph", "content": [{"type": "text", "text": "保留加粗 & 链接", "marks": [{"type": "bold"}, {"type": "link", "attrs": {"href": "https://example.test/?a=1&b=2"}}]}]},
                    {"type": "orderedList", "attrs": {"start": 3}, "content": [{"type": "listItem", "content": [body("第三项")["content"][0]]}]},
                    {"type": "table", "content": [{"type": "tableRow", "content": [{"type": "tableHeader", "content": [body("表头")["content"][0]]}, {"type": "tableCell", "content": [body("单元格")["content"][0]]}]}]},
                    {"type": "image", "attrs": {"attachmentId": image["id"], "name": "pixel.png", "alt": "本地图片"}},
                    {"type": "blockMath", "attrs": {"latex": "x^2"}},
                ]}
                word = a.rpc("desktop/document/create", {"title": "Word/中文", "format": "richtext", "body": rich})["document"]
                a.rpc("desktop/document/update", {"id": word["id"], "expectedRevision": 1, "title": "Newer title", "body": body("newer text")})
                saved_bytes = (store / "desktop.json").read_bytes()
                result = a.rpc("desktop/document/export", {"id": word["id"], "format": "docx", "revision": 1})
                require(result["filename"] == "Word_中文.docx" and "wordprocessingml.document" in result["mimeType"], "historical Word export used the wrong title or MIME type")
                require(any("LaTeX" in warning for warning in result["warnings"]), "Word export hid its equation representation limit")
                exported_bytes = base64.b64decode(result["dataBase64"], validate=True)
                with zipfile.ZipFile(io.BytesIO(exported_bytes)) as package:
                    for name in package.namelist():
                        require(not name.startswith("/") and ".." not in name.split("/"), "Word export contains an unsafe package path")
                        if name.endswith((".xml", ".rels")):
                            ET.fromstring(package.read(name))
                    document_xml = package.read("word/document.xml")
                    require("Word 中文 😀".encode() in document_xml and b"newer text" not in document_xml, "Word export did not pin the requested revision")
                    require(package.read("word/media/image1.png") == png, "Word export changed the embedded PNG bytes")
                    require(b'&amp;b=2' in package.read("word/_rels/document.xml.rels"), "Word export lost or mis-escaped its hyperlink")
                require((store / "desktop.json").read_bytes() == saved_bytes, "Export changed document persistence")
                imported_word = a.rpc("files/import", {"name": result["filename"], "mimeType": result["mimeType"], "dataBase64": result["dataBase64"]})["attachment"]
                require(imported_word["kind"] == "docx" and imported_word["parseStatus"] == "ready", "The exported Word file could not be reimported")
                text = a.rpc("files/read", {"id": imported_word["id"], "includeContent": True})["content"]["text"]
                require("Word 中文 😀" in text and "单元格" in text and "保留加粗 & 链接" in text, "Reimport lost exported Word text or tables")
                if sys.platform == "darwin":
                    decoded = subprocess.run(["/usr/bin/textutil", "-convert", "txt", "-format", "docx", "-stdin", "-stdout"], input=exported_bytes, capture_output=True, timeout=20)
                    require(decoded.returncode == 0 and "Word 中文 😀" in decoded.stdout.decode("utf-8"), "macOS's independent DOCX reader could not read the export")
                a.rpc("files/delete", {"id": image["id"]})
                a.expect_error("desktop/document/export", {"id": word["id"], "format": "docx", "revision": 1})
                a.rpc("desktop/document/delete", {"id": word["id"], "expectedRevision": 2})
                passed("Word export pins revisions, embeds originals, preserves Unicode and reimports through independent readers; missing images fail")

                a.expect_error("desktop/document/delete", {"id": document_id})
                a.expect_error("desktop/document/update", {"id": document_id, "expectedRevision": 6, "body": {"type": "doc", "content": [{"type": "image", "attrs": {"attachmentId": "att_" + "a" * 32, "src": "https://example.test/x"}}]}})
                require(a.rpc("desktop/document/read", {"id": document_id})["document"]["revision"] == 6, "rejected rich mutation changed the revision")
                passed("unsafe image source and tokenless rich deletion rejected")

                parent = a.rpc("desktop/document/create", {"title": "Work parent", "projectId": project["id"], "content": "Parent"})["document"]
                blocks = {"type": "doc", "content": [
                    {"type": "workBlock", "attrs": {"kind": kind, "title": "Local " + kind, "text": "中文 request 🙂", "targetId": parent["id"] if kind in ("page", "chat") else "", "html": "<input><script>document.body.dataset.test='local'</script>" if kind == "visualize" else ""}}
                    for kind in ["prompt", "task", "instructions", "highlight", "page", "chat", "visualize"]
                ]}
                child = a.rpc("desktop/document/create", {"title": "Work child", "parentId": parent["id"], "format": "richtext", "schemaVersion": 2, "body": blocks})["document"]
                require(child["projectId"] == project["id"] and child["parentId"] == parent["id"], "subpage did not inherit the local project")
                a.expect_error("desktop/document/update", {"id": parent["id"], "expectedRevision": 1, "parentId": child["id"]})
                a.expect_error("desktop/document/update", {"id": child["id"], "expectedRevision": 1, "schemaVersion": 1})
                markdown = a.rpc("desktop/document/export", {"id": child["id"], "format": "markdown"})["content"]
                require(markdown.count("```mcodex-block") == 7 and "中文 request 🙂" in markdown, "Work block Markdown export lost structure or Unicode")
                passed("schema v2 Work blocks, local subpage inheritance, cycle and downgrade rejection")

            with server() as a:
                a.initialize(store)
                restored_child = a.rpc("desktop/document/read", {"id": child["id"]})["document"]
                require(restored_child["body"] == blocks and restored_child["schemaVersion"] == 2 and restored_child["parentId"] == parent["id"], "restart lost Work blocks or hierarchy")
                a.rpc("desktop/document/delete", {"id": parent["id"], "expectedRevision": 1})
                detached_child = a.rpc("desktop/document/read", {"id": child["id"]})["document"]
                require(detached_child["parentId"] is None and detached_child["revision"] == 2 and detached_child["body"] == blocks, "parent deletion did not preserve and detach the child")
                a.rpc("desktop/document/delete", {"id": child["id"], "expectedRevision": 2})
                passed("Work blocks and hierarchy survive restart; deleting a parent keeps the child content")
                document = a.rpc("desktop/document/read", {"id": document_id})["document"]
                require(document["body"] == original and document["sourceMarkdown"] == source and document["revision"] == 6, "process restart lost structured content or source")
                require(a.rpc("desktop/document/comments/list", {"id": document_id})["items"][0]["id"] == comment["id"], "process restart lost local comments")
                a.rpc("desktop/project/delete", {"id": project["id"]})
                detached = a.rpc("desktop/document/read", {"id": document_id})["document"]
                require(detached["revision"] == 7 and detached["projectId"] is None, "project detach did not participate in document revisioning")
                restored = a.rpc("desktop/document/restore", {"id": document_id, "revision": 1, "expectedRevision": 7})["document"]
                require(restored["content"] == source and restored["projectId"] is None and restored["revision"] == 8, "restore lost original source or resurrected a deleted project")
                found = a.rpc("desktop/document/list", {"search": "full search needle"})["items"]
                require(len(found) == 1 and found[0]["contentTruncated"] is True and "FULL SEARCH NEEDLE" not in found[0]["content"], "search used only the summary excerpt")
                passed("restart, project detach and full-content search")

                a.rpc("desktop/document/comments/delete", {"id": document_id, "commentId": comment["id"], "expectedRevision": 8})
                a.rpc("desktop/document/delete", {"id": document_id, "expectedRevision": 9})
                a.expect_error("desktop/document/read", {"id": document_id})
                require(a.rpc("desktop/document/list")["items"] == [], "deleted document remains listed")
                passed("comment and document deletion are durable")
        finally:
            for instance in servers:
                if instance.process.poll() is None:
                    instance.close()
    print(f"PASS document smoke: {groups} groups, {sum(instance.rpc_count for instance in servers)} JSONL RPCs, 3 isolated Rust processes, 0 model calls; temporary data removed")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=10)
    arguments = parser.parse_args()
    if not arguments.binary.is_file() or not 0 < arguments.timeout <= 120:
        parser.error("--binary must exist and --timeout must be in (0,120]")
    try:
        run(arguments.binary.resolve(), arguments.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError) as error:
        print(f"FAIL document smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
