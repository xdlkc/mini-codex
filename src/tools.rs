//! Tool definitions and execution: built-in `shell` (optional macOS Seatbelt sandbox) and
//! `apply_patch`, plus MCP and plugin tools.

use crate::agent::{Event, Verdict};
use crate::mcp::Mcp;
use crate::patch;
use crate::plugin::{self, Plugin};
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::{mpsc::UnboundedSender, oneshot};

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

pub struct ToolCtx {
    pub cwd: PathBuf,
    pub sandbox: Sandbox,
    pub auto_approve: bool,
    pub mcp: Arc<Mcp>,
    pub plugins: Vec<Plugin>,
    /// Set once the user chose "approve for session".
    pub session_allow: AtomicBool,
}

pub fn definitions(ctx: &ToolCtx) -> Value {
    let mut defs = vec![
        json!({
            "type": "function",
            "name": "shell",
            "description": "Runs a shell command and returns its output.",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {"type": "array", "items": {"type": "string"}, "description": "argv, e.g. [\"bash\",\"-lc\",\"ls\"]"},
                    "workdir": {"type": "string"},
                    "timeout_ms": {"type": "number"}
                },
                "required": ["command"]
            }
        }),
        json!({
            "type": "function",
            "name": "apply_patch",
            "description": "Edit files using the '*** Begin Patch' format (Add File / Delete File / Update File with @@ hunks).",
            "parameters": {
                "type": "object",
                "properties": {"input": {"type": "string", "description": "The full patch text"}},
                "required": ["input"]
            }
        }),
    ];
    for t in &ctx.mcp.tools {
        defs.push(json!({"type": "function", "name": t.qualified(), "description": t.description, "parameters": t.schema}));
    }
    defs.extend(plugin::definitions(&ctx.plugins));
    Value::Array(defs)
}

async fn approve(
    id: &str,
    name: &str,
    what: String,
    ctx: &ToolCtx,
    tx: &UnboundedSender<Event>,
) -> bool {
    if ctx.auto_approve || ctx.session_allow.load(Ordering::Relaxed) {
        return true;
    }
    let (reply, rx) = oneshot::channel();
    if tx
        .send(Event::Approval {
            id: id.into(),
            name: name.into(),
            what,
            reply,
        })
        .is_err()
    {
        return false;
    }
    match rx.await.unwrap_or(Verdict::No) {
        Verdict::Yes => true,
        Verdict::Always => {
            ctx.session_allow.store(true, Ordering::Relaxed);
            true
        }
        Verdict::No => false,
    }
}

pub async fn run(
    id: &str,
    name: &str,
    args: &str,
    ctx: &ToolCtx,
    tx: &UnboundedSender<Event>,
) -> String {
    match run_inner(id, name, args, ctx, tx).await {
        Ok(s) => s,
        Err(e) => format!("error: {e:#}"),
    }
}

async fn run_inner(
    id: &str,
    name: &str,
    args: &str,
    ctx: &ToolCtx,
    tx: &UnboundedSender<Event>,
) -> Result<String> {
    let v: Value = serde_json::from_str(args)?;
    match name {
        "shell" => {
            let argv: Vec<String> = serde_json::from_value(v["command"].clone())?;
            if argv.is_empty() {
                anyhow::bail!("empty command");
            }
            if !approve(id, name, argv.join(" "), ctx, tx).await {
                return Ok("command rejected by user".into());
            }
            let dir = v["workdir"]
                .as_str()
                .map(|d| ctx.cwd.join(d))
                .unwrap_or_else(|| ctx.cwd.clone());
            let timeout = Duration::from_millis(v["timeout_ms"].as_u64().unwrap_or(10_000));
            exec(&argv, &dir, timeout, ctx).await
        }
        "apply_patch" => {
            let p = v["input"].as_str().unwrap_or_default();
            if ctx.sandbox == Sandbox::ReadOnly {
                return Ok("apply_patch denied: sandbox is read-only".into());
            }
            if !approve(id, name, p.to_string(), ctx, tx).await {
                return Ok("patch rejected by user".into());
            }
            patch::apply(p, &ctx.cwd)
        }
        n if n.starts_with("mcp__") => {
            if !approve(id, name, format!("{n} {args}"), ctx, tx).await {
                return Ok("tool call rejected by user".into());
            }
            ctx.mcp.call(n, v).await
        }
        n if n.starts_with("plugin__") => {
            if !approve(id, name, format!("{n} {args}"), ctx, tx).await {
                return Ok("tool call rejected by user".into());
            }
            plugin::call(&ctx.plugins, n, args).await
        }
        other => Ok(format!("unknown tool: {other}")),
    }
}

pub(crate) async fn exec(
    argv: &[String],
    dir: &PathBuf,
    timeout: Duration,
    ctx: &ToolCtx,
) -> Result<String> {
    let mut cmd = if cfg!(target_os = "macos") && ctx.sandbox != Sandbox::DangerFullAccess {
        let mut profile =
            String::from("(version 1)(allow default)(deny file-write*)(deny network*)");
        if ctx.sandbox == Sandbox::WorkspaceWrite {
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\") (subpath \"/private/tmp\") (subpath \"/private/var/folders\") (subpath \"/dev\"))",
                ctx.cwd.canonicalize().unwrap_or(ctx.cwd.clone()).display()
            ));
        }
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p").arg(profile).args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    };
    cmd.current_dir(dir).kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => Ok(format!("command timed out after {timeout:?}")),
        Ok(out) => {
            let out = out?;
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            if text.len() > 10_000 {
                let mut n = 10_000;
                while !text.is_char_boundary(n) {
                    n -= 1;
                }
                text.truncate(n);
                text.push_str("\n…[truncated]");
            }
            Ok(format!(
                "exit code: {}\n{text}",
                out.status.code().unwrap_or(-1)
            ))
        }
    }
}
