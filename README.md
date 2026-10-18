<p align="center">
  <img src="assets/icon.png" width="128" height="128" alt="Mini Codex Icon" />
</p>

<h1 align="center">Mini Codex (<code>mcodex</code>)</h1>

<p align="center">
  A Rust implementation of the Codex CLI &amp; Native macOS Desktop App (<code>tao</code> + <code>wry</code> + <code>ratatui</code>).
</p>

## Features

- **Native macOS Desktop GUI (`mcodex app` / `Codex.app`)**: Built with `tao` + `wry` WebKit driving an in-process JSON-RPC `app-server` channel, featuring a collapsible sidebar, multi-project threads, reasoning summaries, inline tool/command execution cards, diff/file/terminal inspector drawer, and full settings view.
- **Interactive Terminal UI (`mcodex`)**: Fast `ratatui` + `crossterm` TUI for terminal workflows.
- **Non-interactive Exec (`mcodex exec "..."`)**: Headless execution with Seatbelt sandboxing (`read-only`, `workspace-write`, `danger-full-access`).
- **ChatGPT OAuth &amp; API Key Auth (`mcodex login`)**: Supports browser sign-in or OpenAI API key authentication.
- **MCP &amp; Plugins**: Built-in Model Context Protocol (MCP) client and `~/.mcodex/plugins` loader.

## Build &amp; Run

```bash
# Run terminal TUI
cargo run

# Launch native macOS desktop app
cargo run -- app

# Build macOS .app bundle, .dmg disk image, and .pkg installer
./scripts/package-macos.sh
```
