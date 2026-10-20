# Headless Rust agent

The headless build includes `app-server`, `exec`, the terminal interface, and the loopback browser `preview`. It does not compile tao, WebKit/GTK, wry, muda, or native window code. The desktop build remains the default.

```sh
cargo build --release --locked --no-default-features --bin mcodex
target/release/mcodex --version
target/release/mcodex -C /absolute/project app-server
```

`app-server` reads one JSON object per stdin line and writes responses/notifications as JSONL on stdout. Diagnostic messages use stderr. The initialization request is `{"id":1,"method":"initialize","params":{}}`. It uses the same thread/turn/tool/approval protocol as the desktop. No GUI event loop is started when stdin/stdout are redirected. `mcodex app` returns a clear error in this build.

Use `MCODEX_HOME` to choose the agent's persistent state directory. Its default is `~/.mcodex` on the machine running the agent. Set `MCODEX_NO_CODEX_IMPORT=1` to disable imports from official Codex storage. Credentials are configured separately on that machine. Archives never include accounts, credentials, sessions, or local settings.

## Packages

Run `scripts/package-agent.sh` on the build machine. The script needs Bash, Python 3, tar support from Python's standard library, and a working Rust toolchain. It does not contact another host or install a system service.

```sh
scripts/package-agent.sh                         # Actual host target, release binary + source
scripts/package-agent.sh --debug --offline       # Fast build using cached crates
scripts/package-agent.sh --target x86_64-unknown-linux-gnu
scripts/package-agent.sh --source-only           # Source only; does not claim to build a binary
```

`--target` is passed to Cargo. The matching Rust target, linker, and system libraries must be installed. A failed cross-build produces no new binary package. An archive built on macOS is labelled with its actual Darwin target; renaming an archive does not produce a Linux executable. `--source-only` cannot be combined with `--target`.

Outputs are written to `dist/agent` unless `--output DIR` is given:

- `mini-codex-agent-VERSION-TARGET.tar.gz`: `bin/mcodex` (or `.exe`), an internal manifest, and notices.
- `mini-codex-agent-source-VERSION.tar.gz`: source, `Cargo.lock`, offline frontend assets, and build instructions.
- The matching `.json` manifest records version, exact binary target, profile, binary/archive SHA-256, Rust version, and source revision/dirty state.

For a remote machine without a matching binary, transfer the source archive, extract it into a new directory, and run the first build command on that machine. This needs Rust/Cargo and access to its crate cache or registry; it needs no GTK or WebKit development packages. The source archive contains frontend files because the same headless binary can serve browser `preview`.

An installer or SSH controller should verify the archive SHA-256 against the external manifest before extraction, then verify `bin/mcodex` against the internal manifest. After that check, install the extracted executable in a versioned user directory. For example, from inside an extracted binary package:

```sh
AGENT_INSTALL_DIR="$HOME/.local/share/mini-codex/agents/0.2.0/x86_64-apple-darwin"
install -d "$AGENT_INSTALL_DIR"
install -m 755 bin/mcodex "$AGENT_INSTALL_DIR/mcodex"
MCODEX_HOME="$HOME/.mcodex-remote" "$AGENT_INSTALL_DIR/mcodex" -C /absolute/project app-server
```

Use the version and target from the manifest, and an existing absolute project path on that machine. For SSH, run the same final command through SSH stdio. This packaging script does not perform that installation, create an SSH connection, or register a daemon.

Desktop builds and signed bundles still use `cargo build` and `scripts/package-macos.sh`. To re-enable native windows explicitly, use `--features desktop`.
