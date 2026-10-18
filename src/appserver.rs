//! `mcodex app-server`: JSON-RPC over stdio (JSONL, no "jsonrpc" field) implementing a subset of the
//! Codex app-server protocol, so GUI/IDE clients can drive the agent. Method and notification names,
//! item shapes and approval requests follow the schema emitted by `codex app-server generate-json-schema`.

use crate::agent::{Agent, Event, Verdict};
use crate::session::{self, Session};
use crate::tools::{Sandbox, ToolCtx};
use crate::{api, auth, config, mcp, plugin};
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

type RpcResult = std::result::Result<Value, (i64, String)>;

pub struct Shared {
    pub cfg: config::Config,
    pub default_model: String,
    pub default_sandbox: Sandbox,
    pub default_cwd: PathBuf,
    pub yolo: bool,
    pub mcp: Arc<mcp::Mcp>,
}

struct Thread {
    agent: Arc<AsyncMutex<Agent>>,
    runner: Mutex<Option<JoinHandle<()>>>,
    interrupted: Arc<std::sync::atomic::AtomicBool>,
    turn_id: Mutex<Option<String>>,
    cwd: String,
    created_ms: u64,
}

struct Server {
    out: UnboundedSender<Value>,
    shared: Shared,
    threads: Mutex<HashMap<String, Arc<Thread>>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    next_id: AtomicU64,
    ids: AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn sandbox_from(s: &str) -> Option<Sandbox> {
    match s {
        "read-only" | "readOnly" => Some(Sandbox::ReadOnly),
        "workspace-write" | "workspaceWrite" => Some(Sandbox::WorkspaceWrite),
        "danger-full-access" | "dangerFullAccess" => Some(Sandbox::DangerFullAccess),
        _ => None,
    }
}

fn sandbox_policy(s: Sandbox, cwd: &str) -> Value {
    match s {
        Sandbox::ReadOnly => json!({"type": "readOnly"}),
        Sandbox::WorkspaceWrite => json!({"type": "workspaceWrite", "writableRoots": [cwd]}),
        Sandbox::DangerFullAccess => json!({"type": "dangerFullAccess"}),
    }
}

fn thread_json(id: &str, name: Option<&str>, preview: &str, cwd: &str, model: &str, created_ms: u64, updated_ms: u64, active: bool, turns: Vec<Value>) -> Value {
    json!({
        "id": id, "sessionId": id, "name": name, "preview": preview, "cwd": cwd, "model": model,
        "modelProvider": "openai", "cliVersion": env!("CARGO_PKG_VERSION"), "ephemeral": false,
        "createdAt": created_ms / 1000, "updatedAt": updated_ms / 1000, "projectId": null,
        "source": "appServer", "originator": "mcodex",
        "status": if active { json!({"type": "active", "activeFlags": []}) } else { json!({"type": "idle"}) },
        "turns": turns,
    })
}

/// Rebuild protocol turns/items from the stored Responses-API history.
fn history_to_turns(items: &[Value], cwd: &str) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    let mut call_pos: HashMap<String, (usize, usize)> = HashMap::new();
    for (n, i) in items.iter().enumerate() {
        let ty = i["type"].as_str().unwrap_or("");
        if ty == "message" && i["role"] == "user" {
            let text: String = i["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect()).unwrap_or_default();
            turns.push(json!({"id": format!("turn_{n}"), "status": "completed", "items": [
                {"type": "userMessage", "id": format!("item_{n}"), "content": [{"type": "text", "text": text, "text_elements": []}]}]}));
            continue;
        }
        let ti = turns.len().saturating_sub(1);
        let Some(turn) = turns.last_mut() else { continue };
        let list = turn["items"].as_array_mut().unwrap();
        match ty {
            "message" => {
                let text: String = i["content"].as_array().map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect()).unwrap_or_default();
                list.push(json!({"type": "agentMessage", "id": format!("item_{n}"), "text": text, "phase": i.get("phase").cloned().unwrap_or(Value::Null)}));
            }
            "reasoning" => {
                let summary_parts: Vec<String> = i["summary"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|s| s["text"].as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let summary_text = summary_parts.join("\n\n");
                if !summary_text.trim().is_empty() {
                    let rid = i["id"].as_str().map(String::from).unwrap_or_else(|| format!("rs_{n}"));
                    list.push(json!({
                        "type": "reasoning",
                        "id": rid,
                        "summary": summary_parts,
                        "text": summary_text,
                        "status": "completed",
                    }));
                }
            }
            "function_call" => {
                let call_id = i["call_id"].as_str().unwrap_or("").to_string();
                let mut item = tool_item(&call_id, i["name"].as_str().unwrap_or(""), i["arguments"].as_str().unwrap_or("{}"), cwd);
                if let Some(pc) = i.get("parsed_cmd").and_then(|v| v.as_array()) {
                    if !pc.is_empty() {
                        item["commandActions"] = json!(pc);
                    }
                }
                if let Some(dur) = i.get("duration_ms").and_then(|v| v.as_u64()) {
                    item["durationMs"] = json!(dur);
                }
                list.push(item);
                call_pos.insert(call_id, (ti, list.len() - 1));
            }
            "function_call_output" => {
                if let Some(&(t, p)) = call_pos.get(i["call_id"].as_str().unwrap_or("")) {
                    let it = &mut turns[t]["items"][p];
                    finish_tool_item(it, i["output"].as_str().unwrap_or(""));
                }
            }
            _ => {}
        }
    }
    turns
}

/// Classify a shell command string into semantic actions (`read`, `search`, `list_files`, `unknown`)
/// matching Codex / ChatGPT.app's `parsed_cmd` / `commandActions` schema.
fn classify_shell_cmd(raw: &str) -> Vec<Value> {
    let mut inner = raw.trim();
    for prefix in ["/bin/zsh -lc ", "/bin/bash -lc ", "zsh -lc ", "bash -lc ", "sh -lc ", "/bin/sh -lc "] {
        if let Some(rest) = inner.strip_prefix(prefix) {
            inner = rest.trim();
            if (inner.starts_with('\'') && inner.ends_with('\'')) || (inner.starts_with('"') && inner.ends_with('"')) {
                inner = &inner[1..inner.len() - 1];
            }
            break;
        }
    }
    let mut actions = Vec::new();
    for seg in inner.split("&&").flat_map(|s| s.split(';')) {
        let s = seg.trim();
        if s.is_empty() {
            continue;
        }
        let tokens: Vec<&str> = s.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        let mut idx = 0;
        if tokens[idx] == "cd" && tokens.len() >= 2 {
            continue;
        }
        if tokens[idx] == "git" && tokens.get(idx + 1) == Some(&"-C") && tokens.len() >= 4 {
            idx += 3;
        }
        let bin = tokens[idx].rsplit('/').next().unwrap_or(tokens[idx]);
        match bin {
            "cat" | "head" | "tail" | "sed" | "bat" | "nl" => {
                // Find the file path argument (last non-flag token that isn't a sed script like '1,100p')
                let file_arg = tokens[idx + 1..]
                    .iter()
                    .rev()
                    .copied()
                    .find(|t| !t.starts_with('-') && !t.ends_with('p') && !t.starts_with('\'') && !t.starts_with('"'))
                    .or_else(|| tokens.last().copied())
                    .unwrap_or("")
                    .trim_matches(|c| c == '\'' || c == '"');
                if !file_arg.is_empty() {
                    let name = file_arg.rsplit('/').next().unwrap_or(file_arg);
                    actions.push(json!({"type": "read", "cmd": s, "name": name, "path": file_arg}));
                    continue;
                }
            }
            "rg" | "grep" | "ag" => {
                if tokens[idx + 1..].contains(&"--files") {
                    let path = tokens[idx + 1..]
                        .iter()
                        .rev()
                        .copied()
                        .find(|t| !t.starts_with('-'))
                        .map(|t| t.trim_matches(|c| c == '\'' || c == '"'));
                    actions.push(json!({"type": "list_files", "cmd": s, "path": path}));
                    continue;
                } else {
                    let non_flags: Vec<&str> = tokens[idx + 1..].iter().copied().filter(|t| !t.starts_with('-')).collect();
                    let query = non_flags.first().map(|t| t.trim_matches(|c| c == '\'' || c == '"')).unwrap_or("");
                    let path = if non_flags.len() > 1 {
                        Some(non_flags.last().unwrap().trim_matches(|c| c == '\'' || c == '"'))
                    } else {
                        None
                    };
                    actions.push(json!({"type": "search", "cmd": s, "query": query, "path": path}));
                    continue;
                }
            }
            "ls" | "find" | "tree" | "fd" => {
                let path = tokens[idx + 1..]
                    .iter()
                    .copied()
                    .find(|t| !t.starts_with('-'))
                    .map(|t| t.trim_matches(|c| c == '\'' || c == '"'));
                actions.push(json!({"type": "list_files", "cmd": s, "path": path}));
                continue;
            }
            _ => {}
        }
        actions.push(json!({"type": "unknown", "cmd": s}));
    }
    if actions.is_empty() {
        actions.push(json!({"type": "unknown", "cmd": inner}));
    }
    actions
}

/// Protocol item describing a tool call that is starting.
fn tool_item(id: &str, name: &str, args: &str, cwd: &str) -> Value {
    let a: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    match name {
        "shell" | "exec_command" => {
            let cmd = a["command"]
                .as_array()
                .map(|c| c.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" "))
                .or_else(|| a["cmd"].as_str().map(String::from))
                .or_else(|| a["command"].as_str().map(String::from))
                .unwrap_or_default();
            let actions = classify_shell_cmd(&cmd);
            json!({"type": "commandExecution", "id": id, "command": cmd, "commandActions": actions, "cwd": a["workdir"].as_str().unwrap_or(cwd),
                "status": "inProgress", "aggregatedOutput": null, "exitCode": null, "durationMs": null})
        }
        "apply_patch" => json!({"type": "fileChange", "id": id, "changes": patch_changes(a["input"].as_str().unwrap_or("")), "status": "inProgress"}),
        n if n.starts_with("mcp__") => {
            let mut p = n.splitn(3, "__").skip(1);
            json!({"type": "mcpToolCall", "id": id, "server": p.next().unwrap_or(""), "tool": p.next().unwrap_or(""), "arguments": a, "status": "inProgress"})
        }
        n => json!({"type": "dynamicToolCall", "id": id, "tool": n, "arguments": a, "status": "inProgress"}),
    }
}

fn finish_tool_item(item: &mut Value, output: &str) {
    let declined = output.contains("rejected by user") || output.contains("denied");
    let failed = output.starts_with("error:");
    match item["type"].as_str().unwrap_or("") {
        "commandExecution" => {
            let code = output.strip_prefix("exit code: ").and_then(|r| r.lines().next()).and_then(|c| c.trim().parse::<i64>().ok());
            item["aggregatedOutput"] = json!(output.split_once('\n').map(|x| x.1).unwrap_or(output));
            item["exitCode"] = json!(code);
            item["status"] = json!(if declined { "declined" } else if failed || code.unwrap_or(0) != 0 { "failed" } else { "completed" });
        }
        "fileChange" => item["status"] = json!(if declined { "declined" } else if failed { "failed" } else { "completed" }),
        "mcpToolCall" => {
            item["status"] = json!(if failed { "failed" } else { "completed" });
            item["result"] = json!({"content": [{"type": "text", "text": output}], "structuredContent": null});
        }
        _ => {
            item["status"] = json!(if failed { "failed" } else { "completed" });
            item["success"] = json!(!failed);
            item["contentItems"] = json!([{"type": "inputText", "text": output}]);
        }
    }
}

fn patch_changes(patch: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for l in patch.lines() {
        if let Some(p) = l.strip_prefix("*** Add File: ") {
            out.push(json!({"path": p.trim(), "kind": {"type": "add"}, "diff": ""}));
        } else if let Some(p) = l.strip_prefix("*** Delete File: ") {
            out.push(json!({"path": p.trim(), "kind": {"type": "delete"}, "diff": ""}));
        } else if let Some(p) = l.strip_prefix("*** Update File: ") {
            out.push(json!({"path": p.trim(), "kind": {"type": "update", "move_path": null}, "diff": ""}));
        } else if l.starts_with("*** ") {
        } else if let Some(last) = out.last_mut() {
            let d = last["diff"].as_str().unwrap_or("").to_string();
            last["diff"] = json!(format!("{d}{l}\n"));
        }
    }
    out
}

fn workspace_git_status(cwd: &std::path::Path) -> Value {
    let run_git = |args: &[&str]| -> Option<String> {
        let o = std::process::Command::new("git").args(args).current_dir(cwd).output().ok()?;
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).into_owned())
        } else {
            None
        }
    };
    let branch = run_git(&["rev-parse", "--abbrev-ref", "HEAD"]).map(|s| s.trim().to_string());
    let porcelain = run_git(&["status", "--porcelain"]).unwrap_or_default();
    let numstat = run_git(&["diff", "--numstat", "HEAD"]).or_else(|| run_git(&["diff", "--numstat"])).unwrap_or_default();
    let diff = run_git(&["diff", "HEAD"]).or_else(|| run_git(&["diff"])).unwrap_or_default();

    let mut stats_map: HashMap<String, (u64, u64)> = HashMap::new();
    let (mut total_added, mut total_deleted) = (0u64, 0u64);
    for line in numstat.lines() {
        let mut parts = line.split('\t');
        let a = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let d = parts.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if let Some(p) = parts.next() {
            total_added += a;
            total_deleted += d;
            stats_map.insert(p.to_string(), (a, d));
        }
    }
    let mut files = Vec::new();
    for line in porcelain.lines() {
        if line.len() < 4 {
            continue;
        }
        let status = line[..2].trim().to_string();
        let path = line[3..].trim().to_string();
        let (a, d) = stats_map.get(&path).copied().unwrap_or((0, 0));
        files.push(json!({"path": path, "status": status, "added": a, "deleted": d}));
    }
    let diff_trunc: String = diff.chars().take(48_000).collect();
    json!({
        "branch": branch,
        "changedFiles": files,
        "totalAdded": total_added,
        "totalDeleted": total_deleted,
        "diff": diff_trunc,
    })
}

fn list_workspace_files(cwd: &std::path::Path, query: &str) -> Value {
    let mut files = Vec::new();
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" || name == "dist" || name == "build" {
                continue;
            }
            let rel = path.strip_prefix(cwd).unwrap_or(&path).to_string_lossy().to_string();
            let is_dir = path.is_dir();
            if is_dir {
                if stack.len() < 64 {
                    stack.push(path);
                }
            } else if query.is_empty() || rel.to_lowercase().contains(query) {
                files.push(json!({"path": rel, "name": name, "isDir": false}));
                if files.len() >= 250 {
                    break;
                }
            }
        }
        if files.len() >= 250 {
            break;
        }
    }
    files.sort_by(|a, b| a["path"].as_str().unwrap_or("").cmp(b["path"].as_str().unwrap_or("")));
    json!({"cwd": cwd.display().to_string(), "files": files})
}

fn codex_dot_dir() -> PathBuf {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")).unwrap_or_else(config::home)
}

fn global_state_path() -> PathBuf {
    codex_dot_dir().join(".codex-global-state.json")
}

fn read_global_state() -> Value {
    std::fs::read_to_string(global_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}))
}

fn write_global_state(v: &Value) {
    let p = global_state_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(s) = serde_json::to_string_pretty(v) {
        let _ = std::fs::write(p, s);
    }
}

fn codex_config_path() -> PathBuf {
    let p = codex_dot_dir().join("config.toml");
    if p.exists() {
        p
    } else {
        config::home().join("config.toml")
    }
}

fn read_codex_config_table() -> toml::Table {
    std::fs::read_to_string(codex_config_path())
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn write_codex_config_table(tbl: &toml::Table) {
    if let Ok(s) = toml::to_string_pretty(tbl) {
        let p = codex_config_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&p, &s);
        let mp = config::home().join("config.toml");
        if mp != p {
            if let Some(parent) = mp.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(mp, s);
        }
    }
}

fn detect_open_in_targets() -> Vec<Value> {
    let candidates = [
        ("finder", "Finder", "fileManager", "/System/Library/CoreServices/Finder.app", "Finder"),
        ("terminal", "Terminal", "terminal", "/System/Applications/Utilities/Terminal.app", "Terminal"),
        ("iterm", "iTerm", "terminal", "/Applications/iTerm.app", "iTerm"),
        ("antigravity", "Antigravity", "editor", "/Applications/Antigravity.app", "Antigravity"),
        ("cursor", "Cursor", "editor", "/Applications/Cursor.app", "Cursor"),
        ("vscode", "VS Code", "editor", "/Applications/Visual Studio Code.app", "Visual Studio Code"),
        ("windsurf", "Windsurf", "editor", "/Applications/Windsurf.app", "Windsurf"),
        ("zed", "Zed", "editor", "/Applications/Zed.app", "Zed"),
        ("xcode", "Xcode", "editor", "/Applications/Xcode.app", "Xcode"),
    ];
    let mut out = Vec::new();
    for (id, label, kind, app_path, app_name) in candidates {
        if std::path::Path::new(app_path).exists() {
            out.push(json!({"id": id, "label": label, "kind": kind, "appName": app_name}));
        }
    }
    out
}

impl Server {
    fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({"method": method, "params": params}));
    }

    fn new_id(&self, prefix: &str) -> String {
        format!("{prefix}_{}", self.ids.fetch_add(1, Ordering::SeqCst))
    }

    async fn make_agent(&self, session: Session, history: Vec<Value>, model: &str, cwd: &str, sandbox: Sandbox, auto: bool) -> Result<Agent> {
        let plugins = plugin::load_all();
        let cwd_p = PathBuf::from(cwd);
        let instructions = crate::build_instructions(&self.shared.cfg, &plugins, &cwd_p);
        let tbl = read_codex_config_table();
        let effort = tbl.get("model_reasoning_effort").and_then(|v| v.as_str()).unwrap_or("medium").to_string();
        Ok(Agent {
            client: api::Client::new(auth::Auth::load()?),
            model: model.into(),
            reasoning_effort: effort,
            instructions,
            ctx: ToolCtx { cwd: cwd_p, sandbox, auto_approve: auto, mcp: self.shared.mcp.clone(), plugins, session_allow: Default::default() },
            history,
            session,
        })
    }

    fn register(&self, id: &str, agent: Agent, cwd: &str, created_ms: u64) -> Arc<Thread> {
        let t = Arc::new(Thread {
            agent: Arc::new(AsyncMutex::new(agent)),
            runner: Mutex::new(None),
            interrupted: Default::default(),
            turn_id: Mutex::new(None),
            cwd: cwd.into(),
            created_ms,
        });
        self.threads.lock().unwrap().insert(id.into(), t.clone());
        t
    }

    fn params_policy(&self, p: &Value) -> (Sandbox, bool) {
        let sb = p["sandbox"].as_str().and_then(sandbox_from).or_else(|| p["sandboxPolicy"]["type"].as_str().and_then(sandbox_from)).unwrap_or(self.shared.default_sandbox);
        let auto = self.shared.yolo || p["approvalPolicy"].as_str() == Some("never");
        (sb, auto)
    }

    async fn handle(self: &Arc<Self>, method: &str, p: Value) -> RpcResult {
        let err = |m: &str| (-32602, m.to_string());
        match method {
            "initialize" => Ok(json!({"userAgent": format!("mcodex/{}", env!("CARGO_PKG_VERSION")), "codexHome": config::home(), "platformFamily": "unix", "platformOs": std::env::consts::OS.replace("macos", "macos")})),
            "thread/start" => {
                let cwd = p["cwd"].as_str().map(String::from).unwrap_or(self.shared.default_cwd.display().to_string());
                let model = p["model"].as_str().map(String::from).unwrap_or(self.shared.default_model.clone());
                let (sb, auto) = self.params_policy(&p);
                let session = Session::create(&cwd, &model).map_err(|e| (-32000, e.to_string()))?;
                let id = session.id.clone();
                let agent = self.make_agent(session, vec![], &model, &cwd, sb, auto).await.map_err(|e| (-32000, e.to_string()))?;
                let t = self.register(&id, agent, &cwd, now_ms());
                let thread = thread_json(&id, None, "", &cwd, &model, t.created_ms, t.created_ms, false, vec![]);
                self.notify("thread/started", json!({"thread": thread}));
                Ok(json!({"approvalPolicy": if auto { "never" } else { "on-request" }, "approvalsReviewer": "user", "cwd": cwd, "model": model,
                    "modelProvider": "openai", "sandbox": sandbox_policy(sb, &cwd), "thread": thread}))
            }
            "thread/resume" => {
                let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?.to_string();
                let (session, items, meta) = Session::open(Some(&id)).map_err(|e| (-32000, e.to_string()))?;
                let cwd = p["cwd"].as_str().or(meta["cwd"].as_str()).unwrap_or(".").to_string();
                let model = p["model"].as_str().or(meta["model"].as_str()).unwrap_or(&self.shared.default_model).to_string();
                let (sb, auto) = self.params_policy(&p);
                let turns = if p["excludeTurns"].as_bool().unwrap_or(false) { vec![] } else { history_to_turns(&items, &cwd) };
                let created = meta["created_ms"].as_u64().unwrap_or(0);
                let name = meta["name"].as_str().map(String::from);
                let sid = session.id.clone();
                let agent = self.make_agent(session, items, &model, &cwd, sb, auto).await.map_err(|e| (-32000, e.to_string()))?;
                self.register(&sid, agent, &cwd, created);
                let thread = thread_json(&sid, name.as_deref(), "", &cwd, &model, created, now_ms(), false, turns);
                Ok(json!({"approvalPolicy": if auto { "never" } else { "on-request" }, "approvalsReviewer": "user", "cwd": cwd, "model": model,
                    "modelProvider": "openai", "sandbox": sandbox_policy(sb, &cwd), "thread": thread}))
            }
            "thread/list" => {
                let limit = p["limit"].as_u64().unwrap_or(100) as usize;
                let cwd_filter = p["cwd"].as_str().map(String::from);
                let term = p["searchTerm"].as_str().map(|s| s.to_lowercase());
                let data: Vec<Value> = session::list()
                    .into_iter()
                    .filter(|s| cwd_filter.as_ref().is_none_or(|c| &s.cwd == c))
                    .filter(|s| term.as_ref().is_none_or(|t| s.preview.to_lowercase().contains(t) || s.name.as_deref().unwrap_or("").to_lowercase().contains(t)))
                    .take(limit)
                    .map(|s| {
                        let active = self.threads.lock().unwrap().get(&s.id).is_some_and(|t| t.runner.lock().unwrap().as_ref().is_some_and(|h| !h.is_finished()));
                        thread_json(&s.id, s.name.as_deref(), &s.preview, &s.cwd, &s.model, s.created_ms, s.updated_ms, active, vec![])
                    })
                    .collect();
                Ok(json!({"data": data, "nextCursor": null}))
            }
            "thread/read" => {
                let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                let (_s, items, meta) = Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                let cwd = meta["cwd"].as_str().unwrap_or(".");
                let turns = if p["includeTurns"].as_bool().unwrap_or(false) { history_to_turns(&items, cwd) } else { vec![] };
                let preview = history_to_turns(&items, cwd).first().and_then(|t| t["items"][0]["content"][0]["text"].as_str().map(|s| s.chars().take(60).collect::<String>())).unwrap_or_default();
                Ok(json!({"thread": thread_json(id, meta["name"].as_str(), &preview, cwd, meta["model"].as_str().unwrap_or(""), meta["created_ms"].as_u64().unwrap_or(0), now_ms(), false, turns)}))
            }
            "thread/name/set" => {
                let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                let name = p["name"].as_str().ok_or_else(|| err("name"))?;
                let (s, _, _) = Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                s.set_name(name);
                self.notify("thread/name/updated", json!({"threadId": id, "threadName": name}));
                Ok(json!({}))
            }
            "thread/delete" | "thread/archive" => {
                let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                let (s, _, _) = Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                s.delete();
                self.threads.lock().unwrap().remove(id);
                self.notify(if method == "thread/delete" { "thread/deleted" } else { "thread/archived" }, json!({"threadId": id}));
                Ok(json!({}))
            }
            "turn/start" => self.turn_start(p).await,
            "turn/interrupt" => {
                let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                let t = self.threads.lock().unwrap().get(id).cloned().ok_or_else(|| err("unknown thread"))?;
                t.interrupted.store(true, Ordering::SeqCst);
                if let Some(h) = t.runner.lock().unwrap().take() {
                    h.abort();
                }
                Ok(json!({}))
            }
            "model/list" => {
                let tbl = read_codex_config_table();
                let cfg_model = tbl.get("model").and_then(|v| v.as_str()).unwrap_or(&self.shared.default_model).to_string();
                let cfg_effort = tbl.get("model_reasoning_effort").and_then(|v| v.as_str()).unwrap_or("medium").to_string();
                let service_tier = tbl.get("service_tier").and_then(|v| v.as_str()).unwrap_or("default").to_string();
                let efforts = json!([
                    {"reasoningEffort": "low", "label": "Light", "description": "Fast responses with light reasoning"},
                    {"reasoningEffort": "medium", "label": "Medium", "description": "Balanced speed and reasoning depth"},
                    {"reasoningEffort": "high", "label": "High", "description": "Deep reasoning for complex coding tasks"},
                    {"reasoningEffort": "xhigh", "label": "Extra High", "description": "Maximum reasoning depth for the toughest problems"},
                    {"reasoningEffort": "persistent", "label": "Persistent", "description": "Sustained multi-step autonomous reasoning"}
                ]);
                let catalog = [
                    ("gpt-6.1-sol", "GPT-6.1 Sol", "Your daily driver for complex coding. Balances frontier intelligence and speed."),
                    ("gpt-6-astra", "GPT-6 Astra", "Frontier multimodal & architectural reasoning model."),
                    ("gpt-5.6-sol", "GPT-5.6 Sol", "High-efficiency coding and refactoring model."),
                    ("gpt-5.5", "GPT-5.5", "General-purpose flagship reasoning model."),
                    ("gpt-5.4", "GPT-5.4", "Fast, reliable coding and debugging model."),
                    ("gpt-5.3-codex", "GPT-5.3 Codex", "Specialized agentic software engineering model."),
                    ("gpt-5.2-codex", "GPT-5.2 Codex", "Proven agentic coding model."),
                    ("gpt-5", "GPT-5", "Standard GPT-5 reasoning model."),
                ];
                let mut data: Vec<Value> = Vec::new();
                for (id, display, desc) in catalog {
                    data.push(json!({
                        "id": id,
                        "model": id,
                        "displayName": display,
                        "description": desc,
                        "hidden": false,
                        "isDefault": id == cfg_model,
                        "defaultReasoningEffort": cfg_effort,
                        "supportedReasoningEfforts": efforts,
                    }));
                }
                if !data.iter().any(|m| m["id"].as_str() == Some(&cfg_model)) {
                    data.insert(0, json!({
                        "id": cfg_model,
                        "model": cfg_model,
                        "displayName": cfg_model,
                        "description": "Custom configured model",
                        "hidden": false,
                        "isDefault": true,
                        "defaultReasoningEffort": cfg_effort,
                        "supportedReasoningEfforts": efforts,
                    }));
                }
                Ok(json!({"data": data, "defaultModel": cfg_model, "defaultReasoningEffort": cfg_effort, "serviceTier": service_tier, "nextCursor": null}))
            }
            "projects/list" => {
                let gs = read_global_state();
                let mut projects: Vec<Value> = Vec::new();
                let mut seen_paths = std::collections::HashSet::new();
                if let Some(lp) = gs.get("local-projects").and_then(|v| v.as_object()) {
                    let order: Vec<String> = gs
                        .get("project-order")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    let mut keys: Vec<String> = order.into_iter().filter(|k| lp.contains_key(k)).collect();
                    for k in lp.keys() {
                        if !keys.contains(k) {
                            keys.push(k.clone());
                        }
                    }
                    for k in keys {
                        if let Some(proj) = lp.get(&k) {
                            let root = proj["rootPaths"].as_array().and_then(|a| a.first()).and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if !root.is_empty() && seen_paths.insert(root.clone()) {
                                projects.push(json!({
                                    "id": proj["id"].as_str().unwrap_or(&k),
                                    "name": proj["name"].as_str().unwrap_or("Project"),
                                    "path": root,
                                    "createdAt": proj["createdAt"],
                                    "exists": std::path::Path::new(&root).exists(),
                                }));
                            }
                        }
                    }
                }
                // Also include any trusted projects from config.toml if local-projects was empty
                if projects.is_empty() {
                    let tbl = read_codex_config_table();
                    if let Some(projs) = tbl.get("projects").and_then(|v| v.as_table()) {
                        for (path, _) in projs {
                            if seen_paths.insert(path.clone()) && std::path::Path::new(path).exists() {
                                let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.clone());
                                projects.push(json!({"id": path, "name": name, "path": path, "exists": true}));
                            }
                        }
                    }
                }
                let selected_id = gs.get("selected-project").and_then(|v| v["projectId"].as_str()).map(String::from);
                let assignments = gs.get("thread-project-assignments").cloned().unwrap_or_else(|| json!({}));
                let pinned = gs.get("pinned-thread-ids").cloned().unwrap_or_else(|| json!([]));
                Ok(json!({
                    "projects": projects,
                    "selectedProjectId": selected_id,
                    "threadProjectAssignments": assignments,
                    "pinnedThreadIds": pinned,
                }))
            }
            "projects/pickFolder" => {
                let prompt = p["prompt"].as_str().unwrap_or("Select a project folder");
                let script = format!("POSIX path of (choose folder with prompt {:?})", prompt);
                let out = std::process::Command::new("osascript").arg("-e").arg(&script).output().map_err(|e| (-32000, e.to_string()))?;
                if !out.status.success() {
                    return Ok(json!({"cancelled": true, "path": null}));
                }
                let chosen = String::from_utf8_lossy(&out.stdout).trim().trim_end_matches('/').to_string();
                let final_path = if chosen.is_empty() { "/".to_string() } else { chosen };
                Ok(json!({"cancelled": false, "path": final_path}))
            }
            "projects/create" => {
                let mode = p["mode"].as_str().unwrap_or("existing"); // "existing" | "new"
                let init_git = p["initGit"].as_bool().unwrap_or(true);
                let target_path = if mode == "new" {
                    let name = p["name"].as_str().unwrap_or("new-project").trim();
                    let parent = p["parentDir"].as_str().map(PathBuf::from).unwrap_or_else(|| {
                        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Code")).unwrap_or_else(|| self.shared.default_cwd.clone())
                    });
                    let full = parent.join(name);
                    std::fs::create_dir_all(&full).map_err(|e| (-32000, format!("Failed to create folder: {e}")))?;
                    if init_git && !full.join(".git").exists() {
                        let _ = std::process::Command::new("git").arg("init").current_dir(&full).output();
                    }
                    full
                } else {
                    let raw = p["path"].as_str().ok_or_else(|| err("path"))?.trim();
                    let full = PathBuf::from(raw);
                    if !full.exists() {
                        std::fs::create_dir_all(&full).map_err(|e| (-32000, format!("Failed to create folder: {e}")))?;
                    }
                    if init_git && p["initGit"].as_bool() == Some(true) && !full.join(".git").exists() {
                        let _ = std::process::Command::new("git").arg("init").current_dir(&full).output();
                    }
                    full
                };
                let path_str = target_path.display().to_string();
                let proj_name = p["name"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
                    .or_else(|| target_path.file_name().map(|n| n.to_string_lossy().to_string()))
                    .unwrap_or_else(|| "Project".to_string());

                let mut gs = read_global_state();
                if !gs["local-projects"].is_object() {
                    gs["local-projects"] = json!({});
                }
                // Check if project already exists for this path
                let mut proj_id = format!("proj-{:x}", now_ms());
                if let Some(lp) = gs["local-projects"].as_object() {
                    for (k, v) in lp {
                        if v["rootPaths"].as_array().and_then(|a| a.first()).and_then(|x| x.as_str()) == Some(&path_str) {
                            proj_id = k.clone();
                            break;
                        }
                    }
                }
                let ts = now_ms();
                gs["local-projects"][&proj_id] = json!({
                    "id": proj_id,
                    "name": proj_name,
                    "rootPaths": [path_str],
                    "createdAt": ts,
                    "updatedAt": ts,
                });
                let mut order: Vec<Value> = gs["project-order"].as_array().cloned().unwrap_or_default();
                if !order.iter().any(|x| x.as_str() == Some(&proj_id)) {
                    order.insert(0, json!(proj_id));
                    gs["project-order"] = json!(order);
                }
                gs["selected-project"] = json!({"type": "local", "projectId": proj_id});
                write_global_state(&gs);

                // Also mark trusted in config.toml
                let mut tbl = read_codex_config_table();
                let projs = tbl.entry("projects").or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let Some(pt) = projs.as_table_mut() {
                    let mut entry = toml::Table::new();
                    entry.insert("trust_level".into(), toml::Value::String("trusted".into()));
                    pt.insert(path_str.clone(), toml::Value::Table(entry));
                }
                write_codex_config_table(&tbl);

                Ok(json!({"id": proj_id, "name": proj_name, "path": path_str}))
            }
            "projects/remove" => {
                let id = p["projectId"].as_str().ok_or_else(|| err("projectId"))?;
                let mut gs = read_global_state();
                if let Some(lp) = gs["local-projects"].as_object_mut() {
                    lp.remove(id);
                }
                if let Some(order) = gs["project-order"].as_array_mut() {
                    order.retain(|x| x.as_str() != Some(id));
                }
                write_global_state(&gs);
                Ok(json!({}))
            }
            "openIn/list" => Ok(json!({"targets": detect_open_in_targets()})),
            "openIn/open" => {
                let target = p["target"].as_str().unwrap_or("finder");
                let path = p["path"].as_str().unwrap_or(".");
                let app_name = match target {
                    "finder" => "Finder",
                    "terminal" => "Terminal",
                    "iterm" => "iTerm",
                    "antigravity" => "Antigravity",
                    "cursor" => "Cursor",
                    "vscode" => "Visual Studio Code",
                    "windsurf" => "Windsurf",
                    "zed" => "Zed",
                    "xcode" => "Xcode",
                    other => other,
                };
                let _ = std::process::Command::new("/usr/bin/open").args(["-a", app_name, path]).spawn();
                Ok(json!({"ok": true}))
            }
            "settings/read" => {
                let tbl = read_codex_config_table();
                let raw_toml = std::fs::read_to_string(codex_config_path()).unwrap_or_default();
                let global_agents_md = std::fs::read_to_string(codex_dot_dir().join("AGENTS.md")).unwrap_or_default();
                let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                let workspace_agents_md = std::fs::read_to_string(cwd.join("AGENTS.md")).unwrap_or_default();
                let json_cfg: Value = serde_json::to_value(&tbl).unwrap_or_else(|_| json!({}));
                Ok(json!({
                    "config": json_cfg,
                    "rawToml": raw_toml,
                    "configPath": codex_config_path().display().to_string(),
                    "globalAgentsMd": global_agents_md,
                    "workspaceAgentsMd": workspace_agents_md,
                }))
            }
            "settings/write" => {
                let mut tbl = read_codex_config_table();
                if let Some(m) = p["model"].as_str() {
                    tbl.insert("model".into(), toml::Value::String(m.into()));
                }
                if let Some(e) = p["reasoningEffort"].as_str() {
                    tbl.insert("model_reasoning_effort".into(), toml::Value::String(e.into()));
                }
                if let Some(st) = p["serviceTier"].as_str() {
                    tbl.insert("service_tier".into(), toml::Value::String(st.into()));
                }
                if let Some(sb) = p["sandboxMode"].as_str() {
                    tbl.insert("sandbox_mode".into(), toml::Value::String(sb.into()));
                }
                if let Some(ap) = p["approvalPolicy"].as_str() {
                    tbl.insert("approval_policy".into(), toml::Value::String(ap.into()));
                }
                if let Some(desktop_updates) = p["desktop"].as_object() {
                    let desk = tbl.entry("desktop").or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let Some(dt) = desk.as_table_mut() {
                        for (k, v) in desktop_updates {
                            if let Some(b) = v.as_bool() {
                                dt.insert(k.clone(), toml::Value::Boolean(b));
                            } else if let Some(n) = v.as_i64() {
                                dt.insert(k.clone(), toml::Value::Integer(n));
                            } else if let Some(s) = v.as_str() {
                                dt.insert(k.clone(), toml::Value::String(s.into()));
                            }
                        }
                    }
                }
                if let Some(raw) = p["rawToml"].as_str() {
                    let parsed = raw.parse::<toml::Table>().map_err(|e| (-32000, format!("Invalid TOML: {e}")))?;
                    tbl = parsed;
                }
                write_codex_config_table(&tbl);
                if let Some(ga) = p["globalAgentsMd"].as_str() {
                    let _ = std::fs::write(codex_dot_dir().join("AGENTS.md"), ga);
                }
                if let Some(wa) = p["workspaceAgentsMd"].as_str() {
                    let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                    let _ = std::fs::write(cwd.join("AGENTS.md"), wa);
                }
                Ok(json!({"ok": true}))
            }
            "mcp/addServer" => {
                let name = p["name"].as_str().ok_or_else(|| err("name"))?.trim();
                let command = p["command"].as_str().ok_or_else(|| err("command"))?.trim();
                let args: Vec<toml::Value> = p["args"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| toml::Value::String(s.into()))).collect())
                    .unwrap_or_default();
                let mut tbl = read_codex_config_table();
                let mcp_tbl = tbl.entry("mcp_servers").or_insert_with(|| toml::Value::Table(toml::Table::new()));
                if let Some(mt) = mcp_tbl.as_table_mut() {
                    let mut srv = toml::Table::new();
                    srv.insert("type".into(), toml::Value::String("stdio".into()));
                    srv.insert("command".into(), toml::Value::String(command.into()));
                    srv.insert("args".into(), toml::Value::Array(args));
                    mt.insert(name.into(), toml::Value::Table(srv));
                }
                write_codex_config_table(&tbl);
                Ok(json!({"ok": true}))
            }
            "mcp/removeServer" => {
                let name = p["name"].as_str().ok_or_else(|| err("name"))?;
                let mut tbl = read_codex_config_table();
                if let Some(mt) = tbl.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) {
                    mt.remove(name);
                }
                write_codex_config_table(&tbl);
                Ok(json!({"ok": true}))
            }
            "account/read" => {
                let account = match auth::Auth::load() {
                    Ok(a) if a.describe() == "ChatGPT" => json!({"type": "chatgpt", "email": null, "planType": "pro"}),
                    Ok(_) => json!({"type": "apiKey"}),
                    Err(_) => Value::Null,
                };
                Ok(json!({"account": account, "requiresOpenaiAuth": true}))
            }
            "config/read" => {
                let tbl = read_codex_config_table();
                let cfg_val: Value = serde_json::to_value(&tbl).unwrap_or_else(|_| json!({"model": self.shared.default_model}));
                Ok(json!({"config": cfg_val, "origins": {}}))
            }
            "mcpServerStatus/list" => {
                let tbl = read_codex_config_table();
                let mut data: Vec<Value> = self.shared.mcp.clients.keys().map(|n| json!({
                    "name": n,
                    "status": "connected",
                    "tools": self.shared.mcp.tools.iter().filter(|t| &t.server == n).map(|t| (t.name.clone(), json!({"name": t.name, "description": t.description, "inputSchema": t.schema}))).collect::<serde_json::Map<_, _>>(),
                    "resources": [],
                    "resourceTemplates": [],
                    "authStatus": "supported"
                })).collect();
                if let Some(mt) = tbl.get("mcp_servers").and_then(|v| v.as_table()) {
                    for (name, cfg) in mt {
                        if !data.iter().any(|d| d["name"].as_str() == Some(name)) {
                            let cmd = cfg.get("command").and_then(|v| v.as_str()).unwrap_or("");
                            data.push(json!({
                                "name": name,
                                "status": "configured",
                                "command": cmd,
                                "tools": {},
                                "resources": [],
                                "resourceTemplates": [],
                                "authStatus": "unsupported"
                            }));
                        }
                    }
                }
                Ok(json!({"data": data, "nextCursor": null}))
            }
            "skills/list" => {
                let plugins = plugin::load_all();
                let mut data: Vec<Value> = plugins.iter().map(|pl| json!({
                    "name": pl.name,
                    "description": pl.dir.display().to_string(),
                    "skills": pl.tools.len(),
                    "enabled": true,
                    "source": "local",
                })).collect();
                let tbl = read_codex_config_table();
                if let Some(pl_tbl) = tbl.get("plugins").and_then(|v| v.as_table()) {
                    for (k, v) in pl_tbl {
                        let enabled = v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true);
                        data.push(json!({
                            "name": k,
                            "description": "Bundled Codex plugin",
                            "skills": 1,
                            "enabled": enabled,
                            "source": "codex-plugin",
                        }));
                    }
                }
                if let Some(sk_arr) = tbl.get("skills").and_then(|v| v.get("config")).and_then(|v| v.as_array()) {
                    for sk in sk_arr {
                        if let Some(path) = sk.get("path").and_then(|v| v.as_str()) {
                            let enabled = sk.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                            let name = std::path::Path::new(path).parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string());
                            data.push(json!({
                                "name": name,
                                "description": path,
                                "skills": 1,
                                "enabled": enabled,
                                "source": "skill-config",
                            }));
                        }
                    }
                }
                Ok(json!({"data": data}))
            }
            "workspace/gitStatus" => {
                let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                Ok(workspace_git_status(&cwd))
            }
            "fs/readDirectory" => {
                let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                let query = p["query"].as_str().unwrap_or("").to_lowercase();
                Ok(list_workspace_files(&cwd, &query))
            }
            "fs/readFile" => {
                let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                let rel = p["path"].as_str().ok_or_else(|| err("path"))?;
                let full = if std::path::Path::new(rel).is_absolute() { PathBuf::from(rel) } else { cwd.join(rel) };
                let content = std::fs::read_to_string(&full).map_err(|e| (-32000, e.to_string()))?;
                let truncated: String = content.chars().take(32_000).collect();
                Ok(json!({"path": rel, "content": truncated}))
            }
            "command/exec" => {
                let cwd = p["cwd"].as_str().map(PathBuf::from).unwrap_or_else(|| self.shared.default_cwd.clone());
                let cmd = p["command"].as_str().ok_or_else(|| err("command"))?;
                let out = std::process::Command::new("/bin/sh")
                    .arg("-lc")
                    .arg(cmd)
                    .current_dir(&cwd)
                    .output()
                    .map_err(|e| (-32000, e.to_string()))?;
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                Ok(json!({"exitCode": out.status.code().unwrap_or(-1), "output": text}))
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    async fn turn_start(self: &Arc<Self>, p: Value) -> RpcResult {
        let bad = |m: &str| (-32602, m.to_string());
        let tid = p["threadId"].as_str().ok_or_else(|| bad("threadId"))?.to_string();
        let t = self.threads.lock().unwrap().get(&tid).cloned().ok_or_else(|| bad("thread not loaded; call thread/start or thread/resume"))?;
        if t.runner.lock().unwrap().as_ref().is_some_and(|h| !h.is_finished()) {
            return Err((-32000, "a turn is already running".into()));
        }
        let mut text = String::new();
        for i in p["input"].as_array().cloned().unwrap_or_default() {
            match i["type"].as_str() {
                Some("text") => text.push_str(i["text"].as_str().unwrap_or("")),
                Some("mention") | Some("skill") => text.push_str(&format!("\n[{}: {}]", i["name"].as_str().unwrap_or(""), i["path"].as_str().unwrap_or(""))),
                Some("localImage") => text.push_str(&format!("\n[image: {}]", i["path"].as_str().unwrap_or(""))),
                _ => {}
            }
        }
        {
            let mut a = t.agent.lock().await;
            if let Some(m) = p["model"].as_str() {
                a.model = m.into();
            }
            if let Some(eff) = p["reasoningEffort"].as_str().or_else(|| p["effort"].as_str()) {
                a.reasoning_effort = eff.into();
            }
            let (sb, auto) = self.params_policy(&p);
            if p.get("sandboxPolicy").is_some() {
                a.ctx.sandbox = sb;
            }
            if p["approvalPolicy"].is_string() {
                a.ctx.auto_approve = auto;
            }
        }
        let turn_id = self.new_id("turn");
        let started = now_ms();
        *t.turn_id.lock().unwrap() = Some(turn_id.clone());
        t.interrupted.store(false, Ordering::SeqCst);
        let turn = |status: &str, ended: Option<u64>| json!({"id": turn_id, "items": [], "status": status, "startedAt": started / 1000,
            "completedAt": ended.map(|e| e / 1000), "durationMs": ended.map(|e| e - started)});

        self.notify("turn/started", json!({"threadId": tid, "turn": turn("inProgress", None)}));
        self.notify("thread/status/changed", json!({"threadId": tid, "status": {"type": "active", "activeFlags": []}}));
        let user_item = json!({"type": "userMessage", "id": self.new_id("item"), "content": [{"type": "text", "text": text, "text_elements": []}]});
        self.notify("item/started", json!({"item": user_item, "startedAtMs": started, "threadId": tid, "turnId": turn_id}));
        self.notify("item/completed", json!({"item": user_item, "completedAtMs": started, "threadId": tid, "turnId": turn_id}));

        let (tx, mut rx) = unbounded_channel::<Event>();
        let agent = t.agent.clone();
        let runner = tokio::spawn(async move { agent.lock().await.turn(&text, &tx).await });
        *t.runner.lock().unwrap() = Some(runner);

        let srv = self.clone();
        let (tid2, turn_id2, cwd, interrupted) = (tid.clone(), turn_id.clone(), t.cwd.clone(), t.interrupted.clone());
        tokio::spawn(async move {
            let mut open_msg: Option<(String, String)> = None;
            let mut open_reasoning: Option<(String, String, u64)> = None;
            let mut items: HashMap<String, (Value, u64)> = HashMap::new();
            let mut error: Option<String> = None;
            let ids = |m: &str| json!({"threadId": tid2, "turnId": turn_id2, "itemId": m});
            let close_msg = |srv: &Server, open: &mut Option<(String, String)>| {
                if let Some((id, text)) = open.take() {
                    srv.notify("item/completed", json!({"item": {"type": "agentMessage", "id": id, "text": text}, "completedAtMs": now_ms(), "threadId": tid2, "turnId": turn_id2}));
                }
            };
            let close_reasoning = |srv: &Server, open: &mut Option<(String, String, u64)>| {
                if let Some((id, text, started_at)) = open.take() {
                    let now = now_ms();
                    srv.notify(
                        "item/completed",
                        json!({
                            "item": {"type": "reasoning", "id": id, "text": text, "summary": [text], "status": "completed", "durationMs": now.saturating_sub(started_at)},
                            "completedAtMs": now,
                            "threadId": tid2,
                            "turnId": turn_id2,
                        }),
                    );
                }
            };
            while let Some(ev) = rx.recv().await {
                match ev {
                    Event::ReasoningStart { id } => {
                        close_msg(&srv, &mut open_msg);
                        close_reasoning(&srv, &mut open_reasoning);
                        let now = now_ms();
                        srv.notify(
                            "item/started",
                            json!({"item": {"type": "reasoning", "id": id, "text": "", "summary": [], "status": "inProgress"}, "startedAtMs": now, "threadId": tid2, "turnId": turn_id2}),
                        );
                        open_reasoning = Some((id, String::new(), now));
                    }
                    Event::ReasoningDelta { id, delta } => {
                        if open_reasoning.is_none() {
                            let now = now_ms();
                            srv.notify(
                                "item/started",
                                json!({"item": {"type": "reasoning", "id": id, "text": "", "summary": [], "status": "inProgress"}, "startedAtMs": now, "threadId": tid2, "turnId": turn_id2}),
                            );
                            open_reasoning = Some((id.clone(), String::new(), now));
                        }
                        if let Some((rid, buf, _)) = open_reasoning.as_mut() {
                            buf.push_str(&delta);
                            let mut params = ids(rid);
                            params["delta"] = json!(delta);
                            srv.notify("item/reasoning/summaryTextDelta", params);
                        }
                    }
                    Event::ReasoningEnd { id, text } => {
                        if let Some((rid, buf, started_at)) = open_reasoning.take() {
                            let final_text = if text.is_empty() { buf } else { text };
                            let now = now_ms();
                            srv.notify(
                                "item/completed",
                                json!({
                                    "item": {"type": "reasoning", "id": rid, "text": final_text, "summary": [final_text], "status": "completed", "durationMs": now.saturating_sub(started_at)},
                                    "completedAtMs": now,
                                    "threadId": tid2,
                                    "turnId": turn_id2,
                                }),
                            );
                        } else if !text.is_empty() {
                            let now = now_ms();
                            srv.notify(
                                "item/completed",
                                json!({
                                    "item": {"type": "reasoning", "id": id, "text": text, "summary": [text], "status": "completed"},
                                    "completedAtMs": now,
                                    "threadId": tid2,
                                    "turnId": turn_id2,
                                }),
                            );
                        }
                    }
                    Event::Text(d) => {
                        close_reasoning(&srv, &mut open_reasoning);
                        if open_msg.is_none() {
                            let id = srv.new_id("msg");
                            srv.notify("item/started", json!({"item": {"type": "agentMessage", "id": id, "text": ""}, "startedAtMs": now_ms(), "threadId": tid2, "turnId": turn_id2}));
                            open_msg = Some((id, String::new()));
                        }
                        let (id, buf) = open_msg.as_mut().unwrap();
                        buf.push_str(&d);
                        let mut params = ids(id);
                        params["delta"] = json!(d);
                        srv.notify("item/agentMessage/delta", params);
                    }
                    Event::ToolStart { id, name, args } => {
                        close_reasoning(&srv, &mut open_reasoning);
                        close_msg(&srv, &mut open_msg);
                        let now = now_ms();
                        let item = tool_item(&id, &name, &args, &cwd);
                        srv.notify("item/started", json!({"item": item, "startedAtMs": now, "threadId": tid2, "turnId": turn_id2}));
                        items.insert(id, (item, now));
                    }
                    Event::ToolEnd { id, output } => {
                        if let Some((mut item, started_at)) = items.remove(&id) {
                            let now = now_ms();
                            item["durationMs"] = json!(now.saturating_sub(started_at));
                            if item["type"] == "commandExecution" {
                                let mut params = ids(&id);
                                params["delta"] = json!(output);
                                srv.notify("item/commandExecution/outputDelta", params);
                            }
                            finish_tool_item(&mut item, &output);
                            srv.notify("item/completed", json!({"item": item, "completedAtMs": now, "threadId": tid2, "turnId": turn_id2}));
                        }
                    }
                    Event::Approval { id, name, what, reply } => {
                        let (method, params) = match name.as_str() {
                            "apply_patch" => ("item/fileChange/requestApproval", json!({"itemId": id, "threadId": tid2, "turnId": turn_id2, "reason": null, "startedAtMs": now_ms()})),
                            _ => ("item/commandExecution/requestApproval", json!({"itemId": id, "threadId": tid2, "turnId": turn_id2, "command": what, "cwd": cwd, "startedAtMs": now_ms()})),
                        };
                        let srv2 = srv.clone();
                        tokio::spawn(async move {
                            let resp = srv2.request(method, params).await;
                            let v = match resp["decision"].as_str() {
                                Some("accept") => Verdict::Yes,
                                Some("acceptForSession") => Verdict::Always,
                                _ => Verdict::No,
                            };
                            let _ = reply.send(v);
                        });
                    }
                    Event::Done => {}
                    Event::Error(e) => error = Some(e),
                }
            }
            // Channel closed: the turn finished, failed, or was aborted.
            close_reasoning(&srv, &mut open_reasoning);
            close_msg(&srv, &mut open_msg);
            let ended = now_ms();
            let was_interrupted = interrupted.load(Ordering::SeqCst);
            if was_interrupted {
                srv.agent_fix(&tid2).await;
            }
            let mut t = json!({"id": turn_id2, "items": [], "startedAt": started / 1000, "completedAt": ended / 1000, "durationMs": ended - started});
            if was_interrupted {
                t["status"] = json!("interrupted");
            } else if let Some(e) = error {
                srv.notify("error", json!({"error": {"message": e}, "threadId": tid2, "turnId": turn_id2, "willRetry": false}));
                t["status"] = json!("failed");
                t["error"] = json!({"message": e});
            } else {
                t["status"] = json!("completed");
            }
            srv.notify("turn/completed", json!({"threadId": tid2, "turn": t}));
            srv.notify("thread/status/changed", json!({"threadId": tid2, "status": {"type": "idle"}}));
        });
        Ok(json!({"turn": turn("inProgress", None)}))
    }

    async fn agent_fix(&self, tid: &str) {
        let t = self.threads.lock().unwrap().get(tid).cloned();
        if let Some(t) = t {
            t.agent.lock().await.fix_dangling();
        }
    }

    /// Sends a server->client request and waits for its result (empty object on error/disconnect).
    async fn request(&self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _ = self.out.send(json!({"id": id, "method": method, "params": params}));
        let r = rx.await.unwrap_or(Value::Null);
        r.get("result").cloned().unwrap_or(json!({}))
    }
}

pub async fn run_channel(shared: Shared, mut rx: tokio::sync::mpsc::UnboundedReceiver<Value>, out: UnboundedSender<Value>) {
    let srv = Arc::new(Server {
        out,
        shared,
        threads: Default::default(),
        pending: Default::default(),
        next_id: AtomicU64::new(1),
        ids: AtomicU64::new(1),
    });
    while let Some(msg) = rx.recv().await {
        match (msg.get("method").and_then(|m| m.as_str()), msg.get("id").cloned()) {
            (Some(method), Some(id)) => {
                let (srv, method, params) = (srv.clone(), method.to_string(), msg["params"].clone());
                tokio::spawn(async move {
                    let reply = match srv.handle(&method, params).await {
                        Ok(r) => json!({"id": id, "result": r}),
                        Err((code, message)) => json!({"id": id, "error": {"code": code, "message": message}}),
                    };
                    let _ = srv.out.send(reply);
                });
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                if let Some(tx) = id.as_u64().and_then(|i| srv.pending.lock().unwrap().remove(&i)) {
                    let _ = tx.send(msg);
                }
            }
            _ => {}
        }
    }
}

pub async fn run(shared: Shared) -> Result<()> {
    let (out, mut out_rx) = unbounded_channel::<Value>();
    let (in_tx, in_rx) = unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(v) = out_rx.recv().await {
            if stdout.write_all(format!("{v}\n").as_bytes()).await.is_err() || stdout.flush().await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                let _ = in_tx.send(msg);
            }
        }
    });
    run_channel(shared, in_rx, out).await;
    let _ = writer.await;
    Ok(())
}
