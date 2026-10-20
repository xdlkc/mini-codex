#!/usr/bin/env python3
"""Exercise real workspace RPCs against disposable Git repositories and a loopback model.

Build first with `cargo build`, then run `python3 scripts/smoke-workspaces.py`.
The existing turn-smoke transport provides the JSONL client and Responses mock.
No credentials, repositories, branches, or worktrees outside the temporary fixture change.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
from typing import Any

sys.dont_write_bytecode = True
_spec = importlib.util.spec_from_file_location("mcodex_workspace_turn_fixture", Path(__file__).with_name("smoke-turns.py"))
assert _spec is not None and _spec.loader is not None
_turns = importlib.util.module_from_spec(_spec)
sys.modules[_spec.name] = _turns
_spec.loader.exec_module(_turns)
SmokeFailure, require, MockResponses = _turns.SmokeFailure, _turns.require, _turns.MockResponses


class WorkspaceServer(_turns.AppServer):
    def expect_error(self, method: str, params: dict[str, Any], contains: str = "") -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps({"id": request_id, "method": method, "params": params}) + "\n")
        self.process.stdin.flush()
        self.rpc_count += 1
        self.pump_until(lambda: request_id in self.replies, method)
        response = self.replies.pop(request_id)
        require("error" in response, f"{method} unexpectedly succeeded: {response}")
        message = response["error"].get("message", "")
        require(contains.lower() in message.lower(), f"{method} rejected for the wrong reason: {message!r}; expected {contains!r}")
        return response["error"]

    def workspace(self, method: str, cwd: Path, **params: Any) -> dict[str, Any]:
        return self.rpc("workspace/" + method, {"cwd": str(cwd), **params})

    def reject(self, method: str, cwd: Path, contains: str = "", **params: Any) -> None:
        self.expect_error("workspace/" + method, {"cwd": str(cwd), **params}, contains)


def git(cwd: Path, *args: str, data: bytes | None = None, allow_error: bool = False) -> bytes:
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    env.update({"GIT_AUTHOR_NAME": "Workspace Smoke", "GIT_AUTHOR_EMAIL": "smoke@localhost", "GIT_COMMITTER_NAME": "Workspace Smoke", "GIT_COMMITTER_EMAIL": "smoke@localhost", "GIT_TERMINAL_PROMPT": "0"})
    command = ["git", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "commit.gpgSign=false", "-C", str(cwd), *args]
    result = subprocess.run(command, input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, timeout=20)
    require(allow_error or result.returncode == 0, f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace')}")
    return result.stdout


def repository(path: Path) -> None:
    path.mkdir()
    git(path, "init", "-b", "main")
    git(path, "config", "core.autocrlf", "false")
    git(path, "config", "core.attributesFile", "/dev/null")
    (path / ".gitignore").write_bytes(b"ignored/\n*.cache\n")
    (path / ".gitattributes").write_bytes(b"*.crlf text eol=lf\n")
    (path / "nested").mkdir()
    for name, content in {"nested/keep.txt": b"nested committed\n", "mixed.txt": b"base\n", "raw.crlf": b"base\n", "deleted.bin": b"\x00\xffbase", "run.sh": b"#!/bin/sh\nexit 0\n"}.items():
        (path / name).write_bytes(content)
    (path / "run.sh").chmod(0o755)
    os.symlink("mixed.txt", path / "tracked-link")
    git(path, "add", ".")
    git(path, "commit", "-m", "fixture HEAD")


def files(path: Path) -> dict[str, tuple[Any, ...]]:
    """Capture raw bytes, permissions and symlink targets without following links."""
    result: dict[str, tuple[Any, ...]] = {}

    def walk(directory: Path) -> None:
        for entry in os.scandir(directory):
            if directory == path and entry.name == ".git":
                continue
            full = Path(entry.path)
            relative = str(full.relative_to(path))
            mode = entry.stat(follow_symlinks=False).st_mode
            if stat.S_ISLNK(mode):
                result[relative] = ("symlink", os.readlink(os.fsencode(full)))
            elif stat.S_ISDIR(mode):
                result[relative] = ("directory", stat.S_IMODE(mode))
                walk(full)
            elif stat.S_ISREG(mode):
                result[relative] = ("file", stat.S_IMODE(mode), full.read_bytes())
            else:
                raise SmokeFailure(f"unexpected special fixture file: {relative}")

    walk(path)
    return result


def fingerprint(path: Path) -> tuple[Any, ...]:
    return (git(path, "rev-parse", "HEAD").strip(), git(path, "symbolic-ref", "--quiet", "--short", "HEAD", allow_error=True).strip(), git(path, "ls-files", "--stage", "-z"), files(path))


def populate_dirty_worktree(path: Path) -> None:
    (path / "mixed.txt").write_bytes(b"staged version\r\n")
    (path / "raw.crlf").write_bytes(b"staged normalized line\r\n")
    (path / "added.bin").write_bytes(b"\x00\xffstaged addition\n")
    git(path, "add", "mixed.txt", "raw.crlf", "added.bin")
    (path / "mixed.txt").write_bytes(b"unstaged version\x00\xfe\r\n")
    (path / "raw.crlf").write_bytes(b"raw working CRLF\r\nsecond line\r\n")
    (path / "added.bin").write_bytes(b"\x00\x80unstaged addition\r\n")
    (path / "deleted.bin").unlink()
    (path / "run.sh").write_bytes(b"#!/bin/sh\nprintf 'working'\n")
    (path / "run.sh").chmod(0o750)
    (path / "quoted 'name\nfile.bin").write_bytes(b"\x00\x81untracked\r\n")
    (path / "quoted 'name\nfile.bin").chmod(0o600)
    (path / "empty directory").mkdir(mode=0o711)
    (path / "private").mkdir(mode=0o750)
    (path / "private/data.bin").write_bytes(b"private\x00\xff")
    (path / "private/data.bin").chmod(0o640)
    (path / "tracked-link").unlink()
    os.symlink(os.fsencode("missing-\udcff-target"), os.fsencode(path / "tracked-link"))
    os.symlink("../outside-do-not-follow", path / "untracked-link")
    (path / "ignored").mkdir()
    (path / "ignored/secret.cache").write_bytes(b"ignored raw\x00\xff\r\n")
    (path / "ignored/secret.cache").chmod(0o600)


def run(binary: Path, timeout: float) -> None:
    checks = 0

    def check(condition: bool, message: str) -> None:
        nonlocal checks
        require(condition, message)
        checks += 1

    with tempfile.TemporaryDirectory(prefix="mcodex-workspace-smoke-") as directory:
        root = Path(directory).resolve()
        source, other, store = root / "source", root / "other", root / "mcodex"
        repository(source)
        repository(other)
        store.mkdir()
        (store / "config.toml").write_text('model = "local-mock-model"\n', encoding="utf-8")
        (source / "mixed.txt").write_bytes(b"source staged bytes\n")
        git(source, "add", "mixed.txt")
        (source / "mixed.txt").write_bytes(b"source unstaged bytes\r\n")
        (source / "source-untracked.bin").write_bytes(b"do not copy\x00\xff")
        (source / "ignored").mkdir()
        (source / "ignored/source.cache").write_bytes(b"do not copy ignored")
        original = fingerprint(source)

        with MockResponses(timeout) as mock, WorkspaceServer(binary, store, source, mock.base_url, timeout) as app:
            initialized = app.rpc("initialize")
            check(initialized.get("codexImportDisabled") is True and Path(initialized["codexHome"]) == store, "app-server did not confirm isolated data")
            app.expect_error("workspace/gitStatus", {}, "cwd")
            app.expect_error("workspace/worktree/list", {"cwd": "."}, "absolute")
            empty = root / "not-git"
            empty.mkdir()
            check(app.workspace("gitStatus", empty)["isGit"] is False, "non-Git status must be explicit")
            app.reject("branch/list", empty, "not a Git")

            branches = app.workspace("branch/list", source)
            check(branches["currentBranch"] == "main" and any(b["name"] == "main" and b["current"] for b in branches["data"]), "current branch was not reported")
            app.workspace("branch/create", source, branch="codex/smoke-safe", switch=False)
            check(fingerprint(source) == original, "creating a branch without switching changed source files/index/HEAD")
            switched = app.workspace("branch/create", other, branch="codex/alternate", switch=True)
            check(switched["branch"] == "codex/alternate", "branch/create did not switch the explicitly selected clean repository")
            (other / "mixed.txt").write_bytes(b"alternate committed value\n")
            git(other, "add", "mixed.txt")
            git(other, "commit", "-m", "alternate branch fixture")
            check(app.workspace("branch/switch", other, branch="main")["branch"] == "main", "branch/switch did not return to main")
            (other / "mixed.txt").write_bytes(b"uncommitted value that must survive\n")
            other_before = fingerprint(other)
            app.reject("branch/switch", other, "overwritten", branch="codex/alternate")
            check(fingerprint(other) == other_before, "branch switch discarded conflicting uncommitted data")
            for branch in ["--force", "../escape", "bad@{name"]:
                app.reject("branch/create", source, branch=branch, switch=False)
            app.reject("gitDiff", source, "Unsafe", path="../outside")

            # Active source turns block branch mutations, but a committed-HEAD worktree is independent.
            slow = mock.plan("slow")
            source_thread = app.rpc("thread/start", {"cwd": str(source / "nested"), "mode": "codex"})["thread"]["id"]
            source_turn = app.turn(source_thread)
            mock.request()
            check(slow.opened.wait(timeout), "local model did not enter the active source turn")
            app.reject("branch/create", source, "active responses", branch="codex/blocked-active")
            app.reject("branch/switch", source, "active responses", branch="codex/smoke-safe")
            check(not git(source, "show-ref", "--verify", "refs/heads/codex/blocked-active", allow_error=True), "active guard created a branch before rejecting")
            created = app.workspace("worktree/create", source / "nested", name="Isolated smoke", branch="codex/smoke-worktree")["worktree"]
            checkout, selected = Path(created["path"]), Path(created["cwd"])
            check(created["managed"] and created["state"] == "active" and selected == checkout / "nested", "worktree metadata lost managed ownership or selected subdirectory")
            check((checkout / "mixed.txt").read_bytes() == b"base\n" and not (checkout / "source-untracked.bin").exists() and not (checkout / "ignored/source.cache").exists(), "worktree copied uncommitted source state")
            check(git(checkout, "rev-parse", "HEAD").strip() == original[0] and fingerprint(source) == original, "worktree creation changed the source checkout or used an uncommitted base")
            app.rpc("turn/interrupt", {"threadId": source_thread, "turnId": source_turn})
            slow.release.set()
            check(app.event("turn/completed", source_thread, source_turn)["turn"]["status"] == "interrupted", "source mock turn did not interrupt")

            # Ownership boundaries reject the primary checkout and manually added worktrees.
            manual = root / "manual-worktree"
            git(source, "worktree", "add", "--detach", str(manual), "HEAD")
            listing = app.workspace("worktree/list", source)["data"]
            for path in [source, manual]:
                record = next(item for item in listing if Path(item["path"]) == path)
                check(record["managed"] is False, f"{path.name} was incorrectly claimed as managed")
                app.reject("worktree/archive", source, id=record["id"])
                check(path.exists(), "unmanaged checkout was removed")
            app.reject("worktree/archive", other, id=created["id"])
            check(checkout.exists(), "foreign repository archive removed a checkout")
            app.reject("worktree/create", source, "already exists", name="Conflicting branch", branch="codex/smoke-safe")
            failed_creation = next(item for item in app.workspace("worktree/list", source)["data"] if item.get("name") == "Conflicting branch")
            check(failed_creation["state"] == "error" and failed_creation.get("error") and not Path(failed_creation["path"]).exists(), "failed creation lost its recoverable ownership/error record")
            check(fingerprint(source) == original, "failed worktree creation changed original data")

            populate_dirty_worktree(checkout)
            dirty = fingerprint(checkout)
            status = app.workspace("gitStatus", selected)
            check(status["stagedDiff"] and status["unstagedDiff"] and status["ignoredCount"] == 1, "status did not distinguish staged, unstaged and ignored data")
            check(any(f["path"] == "quoted 'name\nfile.bin" and f["untracked"] for f in status["changedFiles"]), "NUL-delimited filenames were corrupted")
            staged = app.workspace("gitDiff", checkout, staged=True, path="mixed.txt")["diff"]
            unstaged = app.workspace("gitDiff", checkout, staged=False, path="mixed.txt")["diff"]
            check("staged version" in staged and "Binary files" in unstaged, "staged and unstaged diff RPCs did not select different states")

            # Rejections must leave the complete dirty checkout and index intact.
            app.reject("worktree/archive", source, "ignored", id=created["id"])
            check(fingerprint(checkout) == dirty, "implicit ignored-file refusal changed workspace data")
            git(source, "worktree", "lock", "--reason", "smoke-protect", str(checkout))
            app.reject("worktree/archive", source, "locked", id=created["id"], includeIgnored=True)
            git(source, "worktree", "unlock", str(checkout))
            check(fingerprint(checkout) == dirty, "locked-worktree refusal changed workspace data")
            git(checkout, "update-index", "--assume-unchanged", "run.sh")
            app.reject("worktree/archive", source, "assume-unchanged", id=created["id"], includeIgnored=True)
            git(checkout, "update-index", "--no-assume-unchanged", "run.sh")
            check(fingerprint(checkout) == dirty, "index-flag refusal changed original bytes or index entries")
            nested_repo = checkout / "embedded"
            nested_repo.mkdir()
            git(nested_repo, "init")
            app.reject("worktree/archive", source, "embedded", id=created["id"], includeIgnored=True)
            check((nested_repo / ".git").is_dir(), "embedded repository was discarded")
            shutil.rmtree(nested_repo)
            check(fingerprint(checkout) == dirty, "embedded-repository refusal changed other files")

            active = mock.plan("slow")
            active_thread = app.rpc("thread/start", {"cwd": str(selected), "mode": "codex"})["thread"]["id"]
            active_turn = app.turn(active_thread)
            mock.request()
            check(active.opened.wait(timeout), "local model did not enter the active managed-worktree turn")
            app.reject("worktree/archive", source, "active responses", id=created["id"], includeIgnored=True)
            check(fingerprint(checkout) == dirty, "active-turn guard changed dirty worktree data")
            app.rpc("turn/interrupt", {"threadId": active_thread, "turnId": active_turn})
            active.release.set()
            app.event("turn/completed", active_thread, active_turn)

            archived = app.workspace("worktree/archive", source, id=created["id"], includeIgnored=True)["worktree"]
            snapshot = archived["snapshot"]
            check(archived["state"] == "archived" and not checkout.exists(), "archive did not remove the verified managed checkout")
            check(snapshot["includeIgnored"] is True and snapshot["ignoredExcluded"] == 0, "ignored bytes were not included in the recovery snapshot")
            check(git(source, "rev-parse", snapshot["commit"] + "^").strip().decode() == snapshot["indexCommit"] and git(source, "rev-parse", snapshot["indexCommit"] + "^").strip() == dirty[0], "recovery commits lost original HEAD/index ancestry")
            check(git(source, "cat-file", "-t", snapshot["manifestRef"]).strip() == b"blob", "recovery manifest is not protected by a Git ref")

            checkout.mkdir()
            (checkout / "do-not-overwrite").write_bytes(b"sentinel")
            app.reject("worktree/restore", source, "already exists", id=created["id"])
            check((checkout / "do-not-overwrite").read_bytes() == b"sentinel", "restore overwrote an existing destination")
            shutil.rmtree(checkout)
            git(source, "update-ref", snapshot["manifestRef"], dirty[0].decode())
            app.reject("worktree/restore", source, "manifest ref changed", id=created["id"])
            check(not checkout.exists(), "a changed recovery manifest created an unverified checkout")
            git(source, "update-ref", snapshot["manifestRef"], snapshot["manifestBlob"])

            # Move the old branch elsewhere. Restore must detach at its saved HEAD without moving it.
            tree = git(source, "rev-parse", "HEAD^{tree}").strip().decode()
            advanced = git(source, "commit-tree", tree, "-p", dirty[0].decode(), data=b"advance original branch independently\n").strip().decode()
            old_branch = "refs/heads/" + dirty[1].decode()
            git(source, "update-ref", old_branch, advanced, dirty[0].decode())
            restored = app.workspace("worktree/restore", source, id=created["id"])["worktree"]
            check(restored["state"] == "active" and restored["branch"] is None, "restore must use a detached HEAD")
            expected = (dirty[0], b"", dirty[2], dirty[3])
            check(fingerprint(checkout) == expected, "restore changed raw bytes, symlink targets, file/directory permissions, original index, or saved HEAD")
            check(git(source, "rev-parse", old_branch).strip().decode() == advanced, "restore moved an existing branch")
            check(fingerprint(source) == original, "archive/restore changed the source checkout")
            records = app.workspace("worktree/list", source)["data"]
            check(any(item["id"] == created["id"] and item["state"] == "active" for item in records), "restored worktree disappeared from the managed list")

            # Ignored data can be excluded only when the caller explicitly requests discard.
            discard = app.workspace("worktree/create", source, name="Explicit ignored discard", detached=True)["worktree"]
            discard_path = Path(discard["path"])
            (discard_path / "keep.txt").write_bytes(b"keep untracked")
            (discard_path / "omit.cache").write_bytes(b"explicitly excluded ignored data")
            excluded = app.workspace("worktree/archive", source, id=discard["id"], discardIgnored=True)["worktree"]
            check(excluded["snapshot"]["ignoredExcluded"] == 1, "explicit ignored exclusion was not recorded")
            app.workspace("worktree/restore", source, id=discard["id"])
            check((discard_path / "keep.txt").read_bytes() == b"keep untracked" and not (discard_path / "omit.cache").exists(), "explicit ignored discard affected preserved untracked data")
            check(not mock.errors, f"local Responses mock errors: {mock.errors}")
            rpc_count = app.rpc_count
    print(f"PASS workspace smoke: {checks} checks, {rpc_count} real JSONL RPCs, 2 loopback model mocks; raw Git recovery verified; temporary data removed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path(__file__).resolve().parents[1] / "target/debug/mcodex")
    parser.add_argument("--timeout", type=float, default=20)
    args = parser.parse_args()
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    binary = args.binary.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        print(f"FAIL executable not found: {binary}; run cargo build first", file=sys.stderr)
        return 1
    try:
        run(binary, args.timeout)
    except (SmokeFailure, OSError, ValueError, KeyError, TypeError, StopIteration, subprocess.SubprocessError) as error:
        print(f"FAIL workspace smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
