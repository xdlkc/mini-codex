//! `mcodex app-server`: JSON-RPC over stdio (JSONL, no "jsonrpc" field) implementing a subset of the
//! Codex app-server protocol, so GUI/IDE clients can drive the agent. Method and notification names,
//! item shapes and approval requests follow the schema emitted by `codex app-server generate-json-schema`.

use crate::agent::{Agent, Event, Verdict};
use crate::session::{self, Session};
use crate::tools::{Sandbox, ToolCtx};
use crate::{
    account, api, attachments, auth, config, desktop, document_ai, goals, images, mcp, models,
    plugin, remote, scheduler, voice, workspaces,
};
#[path = "scheduled_runtime.rs"]
mod scheduled_runtime;
#[path = "voice_runtime.rs"]
mod voice_runtime;
use crate::web_search::SearchMode;
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
    start_lock: AsyncMutex<()>,
    runner: Mutex<Option<JoinHandle<()>>>,
    live: Mutex<Option<LiveTurn>>,
    interrupted: Arc<std::sync::atomic::AtomicBool>,
    turn_id: Mutex<Option<String>>,
    cwd: String,
    created_ms: u64,
    queued_inputs: Mutex<std::collections::VecDeque<Value>>,
    voice_call: Mutex<Option<String>>,
    sandbox: Mutex<Sandbox>,
}

struct Server {
    closing: std::sync::atomic::AtomicBool,
    out: UnboundedSender<Value>,
    shared: Shared,
    threads: Mutex<HashMap<String, Arc<Thread>>>,
    pending: Mutex<HashMap<u64, PendingApproval>>,
    next_id: AtomicU64,
    scheduled_jobs: Mutex<HashMap<String, JoinHandle<()>>>,
    automation_error: Mutex<Option<String>>,
    workspace_gate: AsyncMutex<()>,
    account: account::Account,
    voice: voice::Voice,
    voice_bindings: Mutex<HashMap<String, Arc<VoiceBinding>>>,
    models: models::Catalog,
    remote: remote::RemoteHub,
    images: images::Images,
    document_ai: document_ai::DocumentAi,
}

struct VoiceBinding {
    thread_id: String,
    saved: AsyncMutex<Vec<Value>>,
    ended: std::sync::atomic::AtomicBool,
    created_ms: u64,
}

struct PendingApproval {
    thread_id: String,
    method: String,
    params: Value,
    reply: oneshot::Sender<Value>,
}

#[derive(Clone)]
struct LiveTurn {
    history_len: usize,
    turn: Value,
}

impl LiveTurn {
    fn new(history_len: usize, mut turn: Value) -> Self {
        turn["items"] = json!([]);
        Self { history_len, turn }
    }

    fn is_active(&self) -> bool {
        self.turn["status"] == "inProgress"
    }

    fn apply(&mut self, method: &str, params: &Value) {
        let turn_id = params["turnId"]
            .as_str()
            .or_else(|| params["turn"]["id"].as_str());
        if turn_id != self.turn["id"].as_str() {
            return;
        }
        match method {
            "turn/started" | "turn/completed" => {
                if let Some(fields) = params["turn"].as_object() {
                    for (key, value) in fields {
                        if key != "items" {
                            self.turn[key] = value.clone();
                        }
                    }
                }
            }
            "item/started" | "item/completed" => {
                let mut item = params["item"].clone();
                if !item.is_object() {
                    return;
                }
                if item.get("status").is_none() {
                    item["status"] = json!(if method == "item/started" {
                        "inProgress"
                    } else {
                        "completed"
                    });
                }
                for key in ["startedAtMs", "completedAtMs"] {
                    if let Some(value) = params.get(key) {
                        item[key] = value.clone();
                    }
                }
                let items = self.turn["items"].as_array_mut().unwrap();
                if let Some(existing) = items
                    .iter_mut()
                    .find(|existing| existing["id"] == item["id"])
                {
                    *existing = item;
                } else {
                    items.push(item);
                }
            }
            "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/commandExecution/outputDelta" => {
                let Some(id) = params["itemId"].as_str() else {
                    return;
                };
                let Some(delta) = params["delta"].as_str() else {
                    return;
                };
                let (kind, field) = match method {
                    "item/commandExecution/outputDelta" => ("commandExecution", "aggregatedOutput"),
                    "item/reasoning/summaryTextDelta" => ("reasoning", "text"),
                    _ => ("agentMessage", "text"),
                };
                let items = self.turn["items"].as_array_mut().unwrap();
                let index = items
                    .iter()
                    .position(|item| item["id"] == id)
                    .unwrap_or_else(|| {
                        items.push(json!({"id":id,"type":kind,"status":"inProgress"}));
                        items.len() - 1
                    });
                let item = &mut items[index];
                item[field] = json!(format!("{}{delta}", item[field].as_str().unwrap_or("")));
                if kind == "reasoning" {
                    item["summary"] = json!([item["text"]]);
                }
            }
            _ => {}
        }
    }
}

fn turns_with_snapshot(items: &[Value], cwd: &str, live: Option<&LiveTurn>) -> Vec<Value> {
    turns_with_snapshot_ui(items, cwd, live, &Value::Null)
}
fn turns_with_snapshot_ui(
    items: &[Value],
    cwd: &str,
    live: Option<&LiveTurn>,
    meta: &Value,
) -> Vec<Value> {
    if let Some(live) = live.filter(|live| {
        live.is_active() || matches!(live.turn["status"].as_str(), Some("failed" | "interrupted"))
    }) {
        let mut turns =
            history_to_turns_with_ui(&items[..items.len().min(live.history_len)], cwd, meta);
        turns.push(live.turn.clone());
        turns
    } else {
        history_to_turns_with_ui(items, cwd, meta)
    }
}

impl Thread {
    fn is_active(&self) -> bool {
        if self.voice_call.lock().unwrap().is_some() {
            return true;
        }
        if self
            .live
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(LiveTurn::is_active)
        {
            return true;
        }
        self.runner
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|runner| !runner.is_finished())
    }

    fn recovered_turns(&self, items: &[Value], cwd: &str, meta: &Value) -> (bool, Vec<Value>) {
        let live = self.live.lock().unwrap().clone();
        let active = self.voice_call.lock().unwrap().is_some()
            || live.as_ref().is_some_and(LiveTurn::is_active)
            || self
                .runner
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|runner| !runner.is_finished());
        (
            active,
            turns_with_snapshot_ui(items, cwd, live.as_ref(), meta),
        )
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

fn thread_json(
    id: &str,
    name: Option<&str>,
    preview: &str,
    cwd: &str,
    model: &str,
    created_ms: u64,
    updated_ms: u64,
    active: bool,
    turns: Vec<Value>,
) -> Value {
    let mut thread = json!({
        "id": id, "sessionId": id, "name": name, "preview": preview, "cwd": cwd, "model": model,
        "modelProvider": "openai", "cliVersion": env!("CARGO_PKG_VERSION"), "ephemeral": false,
        "createdAt": created_ms / 1000, "updatedAt": updated_ms / 1000, "projectId": null,
        "source": "appServer", "originator": "mcodex",
        "status": if active { json!({"type": "active", "activeFlags": []}) } else { json!({"type": "idle"}) },
        "turns": turns,
    });
    if let Some(Ok(meta)) = desktop::handle("desktop/threadMetadata/read", &json!({"threadId":id}))
    {
        if let Some(obj) = meta["metadata"].as_object() {
            for (key, value) in obj {
                thread[key] = value.clone();
            }
        }
    }
    if thread["mode"].is_null() {
        thread["mode"] = json!("codex");
    }
    thread
}

fn search_preference(params: &Value) -> std::result::Result<Option<SearchMode>, String> {
    let mode = params
        .get("webSearchMode")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| "webSearchMode must be a string".to_string())
                .and_then(SearchMode::parse)
        })
        .transpose()?;
    let legacy = params
        .get("webSearch")
        .map(|value| value.as_bool().ok_or("webSearch must be a boolean"))
        .transpose()?;
    if let (Some(mode), Some(enabled)) = (mode, legacy) {
        if mode.enabled() != enabled {
            return Err("webSearch and webSearchMode disagree".into());
        }
    }
    Ok(mode.or_else(|| {
        legacy.map(|enabled| {
            if enabled {
                SearchMode::Auto
            } else {
                SearchMode::Disabled
            }
        })
    }))
}
fn saved_search_preference(metadata: &Value) -> std::result::Result<SearchMode, String> {
    search_preference(metadata).map(|mode| mode.unwrap_or(SearchMode::Auto))
}
fn search_policy_allowed() -> std::result::Result<bool, String> {
    let state = desktop::handle("desktop/settings/read", &json!({}))
        .ok_or("Search settings service unavailable")??;
    match state["settings"]["preferences"].get("webSearchAllowed") {
        None => Ok(true),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "webSearchAllowed must be a boolean".into()),
    }
}
fn default_search_mode(mode: &str, sandbox: Sandbox) -> std::result::Result<SearchMode, String> {
    if mode != "codex" {
        return Ok(SearchMode::Auto);
    }
    if let Some(value) = read_codex_config_table().get("web_search") {
        return match value.as_str(){Some("cached")=>Ok(SearchMode::Cached),Some("live")=>Ok(SearchMode::Live),Some("disabled")=>Ok(SearchMode::Disabled),_=>Err("This client supports Codex web_search cached, live or disabled; update the setting before using automatic search".into())};
    }
    Ok(if sandbox == Sandbox::DangerFullAccess {
        SearchMode::Live
    } else {
        SearchMode::Cached
    })
}
fn effective_search_mode(
    preferred: SearchMode,
    mode: &str,
    sandbox: Sandbox,
) -> std::result::Result<SearchMode, String> {
    if !search_policy_allowed()? {
        if preferred == SearchMode::Required {
            return Err(
                "Web search is disabled in Search settings. Enable it before requesting a search."
                    .into(),
            );
        }
        return Ok(SearchMode::Disabled);
    }
    if preferred == SearchMode::Auto {
        default_search_mode(mode, sandbox)
    } else {
        Ok(preferred)
    }
}

/// Rebuild protocol turns/items from the stored Responses-API history.
fn history_to_turns(items: &[Value], cwd: &str) -> Vec<Value> {
    let mut turns: Vec<Value> = Vec::new();
    let mut call_pos: HashMap<String, (usize, usize)> = HashMap::new();
    for (n, i) in items.iter().enumerate() {
        let ty = i["type"].as_str().unwrap_or("");
        if ty == "message" && i["role"] == "user" {
            let text: String = i["_mcodex_display"]["text"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    i["content"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect())
                        .unwrap_or_default()
                });
            let mut files = i["content"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|c| c["type"] == "input_image")
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            files.extend(
                i["_mcodex_display"]["attachments"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .cloned(),
            );
            turns.push(json!({"id": format!("turn_{n}"), "status": "completed", "items": [
                {"type": "userMessage", "id": format!("item_{n}"), "internal":crate::agent::is_internal_user(i), "content": [{"type": "text", "text": text, "text_elements": []}],"attachments":files,"source":if i["_mcodex_voice"].is_object(){"voice"}else{"text"},"webSearchMode":i["_mcodex_display"]["webSearchMode"],"effectiveWebSearchMode":i["_mcodex_display"]["effectiveWebSearchMode"],"webSearch":i["_mcodex_display"]["webSearch"]}]}));
            continue;
        }
        if turns.is_empty() && ty == "message" && i["role"] == "assistant" {
            turns.push(json!({"id":format!("turn_{n}"),"status":"completed","items":[]}));
        }
        let ti = turns.len().saturating_sub(1);
        let Some(turn) = turns.last_mut() else {
            continue;
        };
        let list = turn["items"].as_array_mut().unwrap();
        match ty {
            "message" => {
                let text: String = i["content"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|c| c["text"].as_str()).collect())
                    .unwrap_or_default();
                let mut item =
                    crate::web_search::project_message(i).unwrap_or_else(|_| json!({"text":text}));
                item["type"] = json!("agentMessage");
                item["id"] = json!(format!("item_{n}"));
                item["phase"] = i.get("phase").cloned().unwrap_or(Value::Null);
                item["source"] = json!(if i["_mcodex_voice"].is_object() {
                    "voice"
                } else {
                    "text"
                });
                list.push(item);
            }
            "web_search_call" => {
                if let Ok(mut item) = crate::web_search::project_call(i) {
                    item["id"] = json!(format!("web_item_{n}"));
                    list.push(item);
                }
            }
            "reasoning" => {
                let summary_parts: Vec<String> = i["summary"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|s| s["text"].as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let summary_text = summary_parts.join("\n\n");
                if !summary_text.trim().is_empty() {
                    let rid = i["id"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| format!("rs_{n}"));
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
                if ["goal_checkpoint", "scheduled_task_report"]
                    .contains(&i["name"].as_str().unwrap_or(""))
                {
                    continue;
                }
                let call_id = i["call_id"].as_str().unwrap_or("").to_string();
                let mut item = tool_item(
                    &call_id,
                    i["name"].as_str().unwrap_or(""),
                    i["arguments"].as_str().unwrap_or("{}"),
                    cwd,
                );
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
fn history_to_turns_with_ui(items: &[Value], cwd: &str, meta: &Value) -> Vec<Value> {
    let mut turns = history_to_turns(items, cwd);
    for (position, snapshot) in meta["_mcodex_ui_turns"].as_object().into_iter().flatten() {
        let Ok(position) = position.parse::<usize>() else {
            continue;
        };
        if !items
            .get(position)
            .is_some_and(|item| item["type"] == "message" && item["role"] == "user")
        {
            continue;
        }
        if !matches!(snapshot["status"].as_str(), Some("failed" | "interrupted")) {
            continue;
        }
        let Some(turn) = turns
            .iter_mut()
            .find(|turn| turn["id"] == format!("turn_{position}"))
        else {
            continue;
        };
        let Some(fields) = snapshot.as_object() else {
            continue;
        };
        let user = turn["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["type"] == "userMessage")
            .cloned();
        for (key, value) in fields {
            if key != "id" && key != "items" {
                turn[key] = value.clone();
            }
        }
        let mut visible = user.into_iter().collect::<Vec<_>>();
        visible.extend(
            snapshot["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|item| item["type"] != "userMessage")
                .enumerate()
                .map(|(index, item)| {
                    let mut item = item.clone();
                    item["id"] = json!(format!("recovered_{position}_{index}"));
                    item
                }),
        );
        turn["items"] = json!(visible);
    }
    turns
}

fn visible_user_message_index(items: &[Value], index: u64) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            item["type"] == "message"
                && item["role"] == "user"
                && !crate::agent::is_internal_user(item)
        })
        .nth(usize::try_from(index).ok()?)
        .map(|(position, _)| position)
}

fn internal_goal_continuation(params: &Value, goal: Option<&Value>) -> bool {
    params["goalContinuation"] == true
        && goal.is_some_and(|goal| goal["turnsStarted"].as_u64().unwrap_or(0) > 0)
}

fn heartbeat_ids_for_thread(tasks: &[Value], thread_id: &str) -> Vec<String> {
    tasks
        .iter()
        .filter(|task| {
            task["kind"] == "heartbeat"
                && task["targetThreadId"]
                    .as_str()
                    .or_else(|| task["threadId"].as_str())
                    == Some(thread_id)
        })
        .filter_map(|task| task["id"].as_str().map(String::from))
        .collect()
}

/// Classify a shell command string into semantic actions (`read`, `search`, `list_files`, `unknown`)
/// matching Codex / ChatGPT.app's `parsed_cmd` / `commandActions` schema.
fn classify_shell_cmd(raw: &str) -> Vec<Value> {
    let mut inner = raw.trim();
    for prefix in [
        "/bin/zsh -lc ",
        "/bin/bash -lc ",
        "zsh -lc ",
        "bash -lc ",
        "sh -lc ",
        "/bin/sh -lc ",
    ] {
        if let Some(rest) = inner.strip_prefix(prefix) {
            inner = rest.trim();
            if (inner.starts_with('\'') && inner.ends_with('\''))
                || (inner.starts_with('"') && inner.ends_with('"'))
            {
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
                    .find(|t| {
                        !t.starts_with('-')
                            && !t.ends_with('p')
                            && !t.starts_with('\'')
                            && !t.starts_with('"')
                    })
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
                    let non_flags: Vec<&str> = tokens[idx + 1..]
                        .iter()
                        .copied()
                        .filter(|t| !t.starts_with('-'))
                        .collect();
                    let query = non_flags
                        .first()
                        .map(|t| t.trim_matches(|c| c == '\'' || c == '"'))
                        .unwrap_or("");
                    let path = if non_flags.len() > 1 {
                        Some(
                            non_flags
                                .last()
                                .unwrap()
                                .trim_matches(|c| c == '\'' || c == '"'),
                        )
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
                .map(|c| {
                    c.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .or_else(|| a["cmd"].as_str().map(String::from))
                .or_else(|| a["command"].as_str().map(String::from))
                .unwrap_or_default();
            let actions = classify_shell_cmd(&cmd);
            json!({"type": "commandExecution", "id": id, "command": cmd, "commandActions": actions, "cwd": a["workdir"].as_str().unwrap_or(cwd),
                "status": "inProgress", "aggregatedOutput": null, "exitCode": null, "durationMs": null})
        }
        "apply_patch" => {
            json!({"type": "fileChange", "id": id, "changes": patch_changes(a["input"].as_str().unwrap_or("")), "status": "inProgress"})
        }
        n if n.starts_with("mcp__") => {
            let mut p = n.splitn(3, "__").skip(1);
            json!({"type": "mcpToolCall", "id": id, "server": p.next().unwrap_or(""), "tool": p.next().unwrap_or(""), "arguments": a, "status": "inProgress"})
        }
        n => {
            json!({"type": "dynamicToolCall", "id": id, "tool": n, "arguments": a, "status": "inProgress"})
        }
    }
}

fn finish_tool_item(item: &mut Value, output: &str) {
    let declined = output.contains("rejected by user") || output.contains("denied");
    let failed = output.starts_with("error:");
    match item["type"].as_str().unwrap_or("") {
        "commandExecution" => {
            let code = output
                .strip_prefix("exit code: ")
                .and_then(|r| r.lines().next())
                .and_then(|c| c.trim().parse::<i64>().ok());
            item["aggregatedOutput"] =
                json!(output.split_once('\n').map(|x| x.1).unwrap_or(output));
            item["exitCode"] = json!(code);
            item["status"] = json!(if declined {
                "declined"
            } else if failed || code.unwrap_or(0) != 0 {
                "failed"
            } else {
                "completed"
            });
        }
        "fileChange" => {
            item["status"] = json!(if declined {
                "declined"
            } else if failed {
                "failed"
            } else {
                "completed"
            })
        }
        "mcpToolCall" => {
            item["status"] = json!(if failed { "failed" } else { "completed" });
            item["result"] =
                json!({"content": [{"type": "text", "text": output}], "structuredContent": null});
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

fn list_workspace_files(cwd: &std::path::Path, query: &str) -> Value {
    let mut files = Vec::new();
    let mut stack = vec![cwd.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.')
                || name == "target"
                || name == "node_modules"
                || name == "dist"
                || name == "build"
            {
                continue;
            }
            let rel = path
                .strip_prefix(cwd)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
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
    files.sort_by(|a, b| {
        a["path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["path"].as_str().unwrap_or(""))
    });
    json!({"cwd": cwd.display().to_string(), "files": files})
}

fn global_state_path() -> PathBuf {
    config::home().join("projects.json")
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
    config::home().join("config.toml")
}

fn read_codex_config_table() -> toml::Table {std::fs::read_to_string(codex_config_path()).ok().and_then(|text|text.parse().ok()).unwrap_or_default()}
fn effective_config_table() -> toml::Table {config::read_table()}

fn write_codex_config_table(tbl: &toml::Table) -> Result<()> {
    let text = toml::to_string_pretty(tbl)?;
    let path = codex_config_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let temp = path.with_extension("toml.tmp");
    std::fs::write(&temp, text)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

fn detect_open_in_targets() -> Vec<Value> {
    let candidates = [
        (
            "finder",
            "Finder",
            "fileManager",
            "/System/Library/CoreServices/Finder.app",
            "Finder",
        ),
        (
            "terminal",
            "Terminal",
            "terminal",
            "/System/Applications/Utilities/Terminal.app",
            "Terminal",
        ),
        (
            "iterm",
            "iTerm",
            "terminal",
            "/Applications/iTerm.app",
            "iTerm",
        ),
        (
            "antigravity",
            "Antigravity",
            "editor",
            "/Applications/Antigravity.app",
            "Antigravity",
        ),
        (
            "cursor",
            "Cursor",
            "editor",
            "/Applications/Cursor.app",
            "Cursor",
        ),
        (
            "vscode",
            "VS Code",
            "editor",
            "/Applications/Visual Studio Code.app",
            "Visual Studio Code",
        ),
        (
            "windsurf",
            "Windsurf",
            "editor",
            "/Applications/Windsurf.app",
            "Windsurf",
        ),
        ("zed", "Zed", "editor", "/Applications/Zed.app", "Zed"),
        (
            "xcode",
            "Xcode",
            "editor",
            "/Applications/Xcode.app",
            "Xcode",
        ),
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
    fn thread_json(
        &self,
        id: &str,
        name: Option<&str>,
        preview: &str,
        cwd: &str,
        model: &str,
        created_ms: u64,
        updated_ms: u64,
        active: bool,
        turns: Vec<Value>,
    ) -> Value {
        let mut thread = thread_json(
            id, name, preview, cwd, model, created_ms, updated_ms, active, turns,
        );
        let sandbox = self
            .threads
            .lock()
            .unwrap()
            .get(id)
            .map(|thread| *thread.sandbox.lock().unwrap())
            .unwrap_or(self.shared.default_sandbox);
        let preferred = saved_search_preference(&thread);
        match preferred.and_then(|preferred| {
            effective_search_mode(
                preferred,
                thread["mode"].as_str().unwrap_or("codex"),
                sandbox,
            )
            .map(|effective| (preferred, effective))
        }) {
            Ok((preferred, effective)) => {
                thread["webSearchMode"] = json!(preferred.as_str());
                thread["effectiveWebSearchMode"] = json!(effective.as_str());
                thread["webSearch"] = json!(effective.enabled());
            }
            Err(error) => {
                thread["effectiveWebSearchMode"] = json!("disabled");
                thread["webSearch"] = json!(false);
                thread["webSearchError"] = json!(error);
            }
        }
        if thread["webSearchVersion"].is_null() {
            thread["webSearchVersion"] = json!(0);
        }
        thread
    }
    fn notify(&self, method: &str, params: Value) {
        if let Some(id) = params["threadId"].as_str() {
            let thread = self.threads.lock().unwrap().get(id).cloned();
            if let Some(thread) = thread {
                if let Some(live) = thread.live.lock().unwrap().as_mut() {
                    live.apply(method, &params);
                }
            }
        }
        if method == "turn/completed" {
            self.capture_scheduled_completion(&params);
        }
        let _ = self.out.send(json!({"method": method, "params": params}));
    }

    fn new_id(&self, prefix: &str) -> String {
        format!("{prefix}_{:032x}", rand::random::<u128>())
    }

    async fn make_agent(
        &self,
        session: Session,
        history: Vec<Value>,
        model: &str,
        cwd: &str,
        sandbox: Sandbox,
        auto: bool,
    ) -> Result<Agent> {
        let plugins = plugin::load_all();
        let cwd_p = PathBuf::from(cwd);
        let instructions = crate::build_instructions(&self.shared.cfg, &plugins, &cwd_p);
        let tbl = effective_config_table();
        let effort = tbl
            .get("model_reasoning_effort")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(||models::default_effort(model));
        Ok(Agent {
            web_search: SearchMode::Disabled,
            client: match auth::Auth::load() {
                Ok(auth) => api::Client::new(auth),
                Err(_) => api::Client::unauthenticated(),
            },
            model: model.into(),
            reasoning_effort: effort,
            mode: "codex".into(),
            goal_thread_id: None,
            scheduled_run_id: None,
            instructions,
            ctx: ToolCtx {
                cwd: cwd_p,
                sandbox,
                auto_approve: auto,
                mcp: self.shared.mcp.clone(),
                plugins,
                session_allow: Default::default(),
            },
            history: crate::agent::model_history(history),
            session,
        })
    }

    fn register(&self, id: &str, agent: Agent, cwd: &str, created_ms: u64) -> Arc<Thread> {
        let sandbox = agent.ctx.sandbox;
        let t = Arc::new(Thread {
            agent: Arc::new(AsyncMutex::new(agent)),
            start_lock: AsyncMutex::new(()),
            runner: Mutex::new(None),
            live: Mutex::new(None),
            interrupted: Default::default(),
            turn_id: Mutex::new(None),
            cwd: cwd.into(),
            created_ms,
            queued_inputs: Default::default(),
            voice_call: Default::default(),
            sandbox: Mutex::new(sandbox),
        });
        self.threads.lock().unwrap().insert(id.into(), t.clone());
        t
    }

    fn stop_thread(&self, id: &str) -> RpcResult {
        let thread = self
            .threads
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(|| (-32602, "unknown thread".to_string()))?;
        thread.interrupted.store(true, Ordering::SeqCst);
        if let Some(runner) = thread.runner.lock().unwrap().take() {
            runner.abort();
        }
        self.cancel_approvals(Some(id));
        Ok(json!({}))
    }

    fn deletion_heartbeats(&self, id: &str) -> std::result::Result<Vec<String>, (i64, String)> {
        // Validate both dependent stores before deleting the rollout. Mutations
        // still happen only after the native file was successfully deleted.
        goals::get(id).map_err(|error| (-32000, error))?;
        let schedules =
            scheduler::handle("scheduler/list", &json!({"kind":"heartbeat","limit":1000}))
                .ok_or_else(|| (-32000, "Scheduler is unavailable".to_string()))?
                .map_err(|error| (-32000, error))?;
        let tasks = schedules["data"]
            .as_array()
            .ok_or_else(|| (-32000, "Invalid scheduler task list".to_string()))?;
        if tasks.len() >= 1000 {
            return Err((
                -32000,
                "Cannot safely inspect all heartbeat tasks before deleting this conversation"
                    .into(),
            ));
        }
        Ok(heartbeat_ids_for_thread(tasks, id))
    }

    fn clear_deleted_thread_dependencies(&self, id: &str, heartbeats: &[String]) -> RpcResult {
        goals::handle("thread/goal/clear", &json!({"threadId":id}))
            .ok_or_else(|| (-32000, "Goal store is unavailable".to_string()))?
            .map_err(|error| {
                (
                    -32000,
                    format!("Conversation was deleted, but its goal could not be cleared: {error}"),
                )
            })?;
        self.notify("thread/goal/cleared", json!({"threadId":id}));
        for task_id in heartbeats {
            scheduler::handle("scheduler/update", &json!({"id":task_id,"state":"paused"}))
                .ok_or_else(||(-32000,"Scheduler is unavailable".to_string()))?
                .map_err(|error|(-32000,format!("Conversation was deleted, but heartbeat {task_id} could not be paused: {error}")))?;
        }
        if !heartbeats.is_empty() {
            self.notify("scheduler/changed", json!({}));
        }
        Ok(json!({}))
    }

    async fn handle_goal(self: &Arc<Self>, method: &str, mut params: Value) -> RpcResult {
        let id = params["threadId"].as_str().map(String::from);
        if ["thread/goal/set", "thread/goal/clear"].contains(&method) {
            let id = id
                .as_deref()
                .ok_or_else(|| (-32602, "threadId is required".into()))?;
            Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
            let old = goals::get(id).map_err(|e| (-32000, e))?;
            if method == "thread/goal/set" {
                if let Some(objective) = params.get("objective") {
                    let objective = objective
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.trim().chars().count() <= 4000)
                        .ok_or_else(|| {
                            (
                                -32602,
                                "Goal objective must contain 1 to 4,000 characters".into(),
                            )
                        })?;
                    if old.as_ref().is_some_and(|g| {
                        g["status"] != "complete"
                            && g["objective"].as_str() != Some(objective.trim())
                    }) && params["replace"] != true
                        && params["edit"] != true
                    {
                        return Err((-32602, "Confirm replacement of the unfinished goal".into()));
                    }
                }
                if let Some(status) = params.get("status") {
                    if status != "active" && status != "paused" {
                        return Err((-32602, "Goal status must be active or paused".into()));
                    }
                }
                if let Some(budget) = params.get("tokenBudget") {
                    if !budget.is_null() && budget.as_u64().is_none_or(|n| n == 0) {
                        return Err((-32602, "Token budget must be positive or null".into()));
                    }
                }
                if old.is_none() && params["objective"].as_str().is_none() {
                    return Err((-32602, "An objective is required to start a goal".into()));
                }
            }
            let running_goal = old.as_ref().is_some_and(|g| g["inFlight"] == true);
            if running_goal {
                let keep_active = method == "thread/goal/set"
                    && params.get("status").is_none()
                    && old.as_ref().is_some_and(|g| g["status"] == "active");
                if let Some(Ok(value)) =
                    goals::handle("thread/goal/set", &json!({"threadId":id,"status":"paused"}))
                {
                    self.notify(
                        "thread/goal/updated",
                        json!({"threadId":id,"goal":value["goal"]}),
                    );
                }
                self.stop_thread(id)?;
                // Replacement must not let an old model result update the new objective.
                let limit = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let active = self
                        .threads
                        .lock()
                        .unwrap()
                        .get(id)
                        .is_some_and(|thread| thread.is_active());
                    if !active {
                        break;
                    }
                    if tokio::time::Instant::now() >= limit {
                        return Err((
                            -32000,
                            "The previous turn is still stopping; try again".into(),
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                if keep_active {
                    params["status"] = json!("active");
                }
            }
        }
        let result = goals::handle(method, &params)
            .ok_or_else(|| (-32601, format!("method not found: {method}")))?
            .map_err(|e| (-32000, e))?;
        if method == "thread/goal/clear" {
            self.notify("thread/goal/cleared", json!({"threadId":id}));
        } else if method == "thread/goal/set" {
            self.notify(
                "thread/goal/updated",
                json!({"threadId":id,"goal":result["goal"]}),
            );
        }
        Ok(result)
    }

    fn automation_problem(&self, message: String) {
        let mut previous = self.automation_error.lock().unwrap();
        if previous.as_ref() != Some(&message) {
            *previous = Some(message.clone());
            drop(previous);
            self.notify("automation/error", json!({"message":message}));
        }
    }

    fn start_automation_driver(self: &Arc<Self>) -> JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let owner = match scheduler::recover_interrupted() {
                Ok(()) => true,
                Err(error) => {
                    if let Some(server) = weak.upgrade() {
                        server.automation_problem(error);
                    }
                    false
                }
            };
            if owner {
                if let Err(error) = goals::recover_interrupted() {
                    if let Some(server) = weak.upgrade() {
                        server.automation_problem(error);
                    }
                }
            }
            let mut clock = tokio::time::interval(std::time::Duration::from_millis(500));
            clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                clock.tick().await;
                let Some(server) = weak.upgrade() else {
                    break;
                };
                let threads: Vec<_> = server
                    .threads
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(id, thread)| (id.clone(), thread.clone()))
                    .collect();
                for (id, thread) in threads {
                    if thread.is_active() {
                        continue;
                    }
                    let next = thread.queued_inputs.lock().unwrap().pop_front();
                    if let Some(mut params) = next {
                        params["threadId"] = json!(id);
                        if let Err(error) = server.turn_start(params.clone()).await {
                            if error.1.contains("already running") {
                                thread.queued_inputs.lock().unwrap().push_front(params);
                            } else {
                                server.notify(
                                    "error",
                                    json!({"threadId":id,"error":{"message":error.1}}),
                                );
                            }
                        }
                        let count = thread.queued_inputs.lock().unwrap().len();
                        server.notify("thread/queue/updated", json!({"threadId":id,"count":count}));
                    }
                }
                if !owner {
                    continue;
                }
                match scheduler::claim_due(now_ms()) {
                    Ok(claims) => {
                        for claim in claims {
                            if let Err(error) = server.launch_scheduled(claim) {
                                server.automation_problem(error);
                            }
                        }
                    }
                    Err(error) => server.automation_problem(error),
                }
                let pending = match goals::pending() {
                    Ok(pending) => pending,
                    Err(error) => {
                        server.automation_problem(error);
                        continue;
                    }
                };
                for goal in pending {
                    let Some(id) = goal["threadId"].as_str() else {
                        continue;
                    };
                    let loaded = server.threads.lock().unwrap().contains_key(id);
                    if !loaded {
                        if let Err(error) =
                            server.handle("thread/resume", json!({"threadId":id})).await
                        {
                            let _ = goals::handle(
                                "thread/goal/set",
                                &json!({"threadId":id,"status":"paused"}),
                            );
                            server.automation_problem(format!(
                                "Goal conversation is unavailable: {}",
                                error.1
                            ));
                            continue;
                        }
                    }
                    let thread = server.threads.lock().unwrap().get(id).cloned().unwrap();
                    if thread.is_active() || !thread.queued_inputs.lock().unwrap().is_empty() {
                        continue;
                    }
                    let prompt = if goal["turnsStarted"].as_u64().unwrap_or(0) == 0 {
                        goal["objective"].as_str().unwrap_or("").to_string()
                    } else {
                        goal["nextPrompt"].as_str().filter(|p|!p.trim().is_empty()).unwrap_or("Continue toward the entire active goal. Inspect the current evidence, finish remaining work, and verify the complete objective before calling goal_checkpoint. Do not report partial work as complete.").to_string()
                    };
                    if let Err(error)=server.turn_start(json!({"threadId":id,"input":[{"type":"text","text":prompt}],"goalContinuation":true})).await {
                        if !error.1.contains("already running") && !error.1.contains("queued") {server.automation_problem(error.1);}
                    }
                }
            }
        })
    }

    fn params_policy(&self, p: &Value) -> (Sandbox, bool) {
        let settings = read_codex_config_table();
        let configured = settings
            .get("sandbox_mode")
            .or_else(|| settings.get("sandbox"))
            .and_then(|v| v.as_str())
            .and_then(sandbox_from)
            .unwrap_or(self.shared.default_sandbox);
        let sb = p["sandbox"]
            .as_str()
            .and_then(sandbox_from)
            .or_else(|| p["sandboxPolicy"]["type"].as_str().and_then(sandbox_from))
            .unwrap_or(configured);
        let auto = self.shared.yolo
            || p["approvalPolicy"]
                .as_str()
                .or_else(|| settings.get("approval_policy").and_then(|v| v.as_str()))
                == Some("never");
        (sb, auto)
    }

    fn handle<'a>(
        self: &'a Arc<Self>,
        method: &'a str,
        p: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResult> + Send + 'a>> {
        Box::pin(async move {
            let err = |m: &str| (-32602, m.to_string());
            if self.closing.load(Ordering::SeqCst) {
                return Err((-32000, "The app server is closing".into()));
            }
            if let Some(result) = self.document_ai.handle(method, &p).await {
                return result.map_err(|e| (-32000, e));
            }
            if let Some(result) = self.remote.handle(method, &p).await {
                return result.map_err(|e| (-32000, e));
            }
            if let Some(result) = self.images.handle(method, &p).await {
                return result.map_err(|e| (-32000, e));
            }
            if method.starts_with("voice/") {
                return self.handle_voice(method, p).await;
            }
            if let Some(result) = attachments::handle(method, &p).await {
                return result.map_err(|e| (-32000, e));
            }
            if method.starts_with("account/") {
                if method == "account/login/start" && p["type"] == "apiKey" {
                    let result = self
                        .account
                        .handle(method, &p)
                        .await
                        .ok_or_else(|| err("Unknown account method"))?
                        .map_err(|e| (-32000, e))?;
                    tokio::join!(
                        self.document_ai.shutdown(),
                        self.images.shutdown(),
                        self.stop_all_voice()
                    );
                    self.models.invalidate().await;
                    return Ok(result);
                }
                if method == "account/logout" {
                    // Invalidate the persisted connection before another turn can use it.
                    // Existing requests are stopped; a later turn always reloads Auth.
                    let result = self
                        .account
                        .handle(method, &p)
                        .await
                        .ok_or_else(|| err("Unknown account method"))?
                        .map_err(|e| (-32000, e))?;
                    tokio::join!(
                        self.document_ai.shutdown(),
                        self.images.shutdown(),
                        self.stop_all_voice()
                    );
                    self.models.invalidate().await;
                    if let Some(Ok(goals)) =
                        goals::handle("thread/goal/list", &json!({"status":"active"}))
                    {
                        for goal in goals["data"].as_array().into_iter().flatten() {
                            if let Some(id) = goal["threadId"].as_str() {
                                if let Some(Ok(result)) = goals::handle(
                                    "thread/goal/set",
                                    &json!({"threadId":id,"status":"paused"}),
                                ) {
                                    self.notify(
                                        "thread/goal/updated",
                                        json!({"threadId":id,"goal":result["goal"]}),
                                    );
                                }
                            }
                        }
                    }
                    let ids: Vec<_> = self.threads.lock().unwrap().keys().cloned().collect();
                    for id in ids {
                        let _ = self.stop_thread(&id);
                    }
                    return Ok(result);
                }
                if let Some(result) = self.account.handle(method, &p).await {
                    return result.map_err(|error| (-32000, error));
                }
            }
            if method.starts_with("workspace/") {
                let mutation = method == "workspace/branch/switch"
                    || (method == "workspace/branch/create" && p["switch"] != false)
                    || method == "workspace/worktree/archive";
                let _gate = if mutation {
                    Some(self.workspace_gate.lock().await)
                } else {
                    None
                };
                if mutation {
                    let target = if method == "workspace/worktree/archive" {
                        let listing = workspaces::handle("workspace/worktree/list", &p)
                            .await
                            .ok_or_else(|| err("Workspace service unavailable"))?
                            .map_err(|e| (-32000, e))?;
                        listing["data"]
                            .as_array()
                            .and_then(|items| items.iter().find(|item| item["id"] == p["id"]))
                            .and_then(|item| item["path"].as_str())
                            .map(PathBuf::from)
                    } else {
                        let status = workspaces::handle("workspace/gitStatus", &p)
                            .await
                            .ok_or_else(|| err("Workspace service unavailable"))?
                            .map_err(|e| (-32000, e))?;
                        status["repoRoot"].as_str().map(PathBuf::from)
                    };
                    if let Some(target) = target.and_then(|path| path.canonicalize().ok()) {
                        let active = self.threads.lock().unwrap().values().any(|thread| {
                            thread.is_active()
                                && PathBuf::from(&thread.cwd)
                                    .canonicalize()
                                    .is_ok_and(|path| path.starts_with(&target))
                        });
                        if active {
                            return Err((-32000,"Stop active responses in this checkout before changing its branch or archiving it".into()));
                        }
                    }
                }
                if let Some(result) = workspaces::handle(method, &p).await {
                    return result.map_err(|error| (-32000, error));
                }
            }
            if let Some(result) = desktop::handle(method, &p) {
                return result.map_err(|e| (-32000, e));
            }
            if method.starts_with("thread/goal/") && method != "thread/goal/start" {
                return self.handle_goal(method, p).await;
            }
            if method == "scheduler/run" {
                let id = p["id"].as_str().ok_or_else(|| err("id"))?;
                let claim = scheduler::claim_run(id, now_ms()).map_err(|e| (-32000, e))?;
                self.launch_scheduled(claim.clone())
                    .map_err(|e| (-32000, e))?;
                return Ok(claim);
            }
            if method == "scheduler/run/cancel" {
                return self
                    .cancel_scheduled(p["id"].as_str().ok_or_else(|| err("id"))?)
                    .await;
            }
            if let Some(result) = scheduler::handle(method, &p) {
                let result = result.map_err(|e| (-32000, e))?;
                if !["scheduler/list", "scheduler/read", "scheduler/runs"].contains(&method) {
                    self.notify("scheduler/changed", json!({}));
                }
                return Ok(result);
            }
            match method {
                "thread/goal/start" => {
                    p["objective"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.trim().chars().count() <= 4000)
                        .ok_or_else(|| err("Goal objective must contain 1 to 4,000 characters"))?;
                    if let Some(budget) = p.get("tokenBudget") {
                        if !budget.is_null() && budget.as_u64().is_none_or(|n| n == 0) {
                            return Err(err("Token budget must be positive or null"));
                        }
                    }
                    let mut options = p.clone();
                    let mut thread = if let Some(id) = p["threadId"].as_str() {
                        self.handle("thread/resume", json!({"threadId":id})).await?["thread"]
                            .clone()
                    } else {
                        self.handle("thread/start", p.clone()).await?["thread"].clone()
                    };
                    if thread["readOnly"] == true {
                        thread = self
                            .handle("thread/fork", json!({"threadId":thread["id"]}))
                            .await?["thread"]
                            .clone();
                    }
                    options["threadId"] = thread["id"].clone();
                    options["status"] = json!("active");
                    let result = self.handle_goal("thread/goal/set", options).await?;
                    Ok(json!({"goal":result["goal"],"thread":thread}))
                }
                "turn/queue" | "turn/steer" => {
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let thread = self
                        .threads
                        .lock()
                        .unwrap()
                        .get(id)
                        .cloned()
                        .ok_or_else(|| err("unknown thread"))?;
                    if !p["input"].is_array() || p["input"].as_array().unwrap().is_empty() {
                        return Err(err("input is empty"));
                    }
                    let mut queue = thread.queued_inputs.lock().unwrap();
                    if queue.len() >= 16 {
                        return Err((-32000, "The message queue is full".into()));
                    }
                    queue.push_back(p.clone());
                    let count = queue.len();
                    drop(queue);
                    self.notify("thread/queue/updated", json!({"threadId":id,"count":count}));
                    Ok(json!({"queued":true,"position":count}))
                }
                "initialize" => Ok(
                    json!({"userAgent": format!("mcodex/{}", env!("CARGO_PKG_VERSION")), "codexHome": config::home(), "cwd":self.shared.default_cwd, "codexImportDisabled":std::env::var("MCODEX_NO_CODEX_IMPORT").as_deref()==Ok("1"), "platformFamily": "unix", "platformOs": std::env::consts::OS.replace("macos", "macos")}),
                ),
                "thread/start" => {
                    let search = search_preference(&p)
                        .map_err(|error| (-32602, error))?
                        .unwrap_or(SearchMode::Auto);
                    let cwd = p["cwd"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or(self.shared.default_cwd.display().to_string());
                    let model = p["model"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or(self.shared.default_model.clone());
                    let (sb, auto) = self.params_policy(&p);
                    let mode = p["mode"].as_str().unwrap_or("codex");
                    if !["chat", "work", "codex"].contains(&mode) {
                        return Err(err("invalid mode"));
                    }
                    effective_search_mode(search, mode, sb).map_err(|error| (-32000, error))?;
                    let session =
                        Session::create(&cwd, &model).map_err(|e| (-32000, e.to_string()))?;
                    let id = session.id.clone();
                    if let Err(e) = desktop::handle("desktop/threadMetadata/set", &json!({"threadId":id,"mode":mode,"projectId":p["projectId"],"webSearchMode":search.as_str()})).unwrap() {session.delete();return Err((-32000,e));}
                    let agent = self
                        .make_agent(session, vec![], &model, &cwd, sb, auto)
                        .await
                        .map_err(|e| (-32000, e.to_string()))?;
                    let t = self.register(&id, agent, &cwd, now_ms());
                    let thread = self.thread_json(
                        &id,
                        None,
                        "",
                        &cwd,
                        &model,
                        t.created_ms,
                        t.created_ms,
                        false,
                        vec![],
                    );
                    self.notify("thread/started", json!({"thread": thread}));
                    Ok(
                        json!({"approvalPolicy": if auto { "never" } else { "on-request" }, "approvalsReviewer": "user", "cwd": cwd, "model": model,
                    "modelProvider": "openai", "sandbox": sandbox_policy(sb, &cwd), "thread": thread}),
                    )
                }
                "thread/resume" => {
                    let id = p["threadId"]
                        .as_str()
                        .ok_or_else(|| err("threadId"))?
                        .to_string();
                    let (session, items, meta) =
                        Session::open(Some(&id)).map_err(|e| (-32000, e.to_string()))?;
                    let cwd = p["cwd"]
                        .as_str()
                        .or(meta["cwd"].as_str())
                        .unwrap_or(".")
                        .to_string();
                    let model = p["model"]
                        .as_str()
                        .or(meta["model"].as_str())
                        .unwrap_or(&self.shared.default_model)
                        .to_string();
                    let (sb, auto) = self.params_policy(&p);
                    let read_only = session.is_read_only();
                    let created = meta["created_ms"].as_u64().unwrap_or(0);
                    let name = meta["name"].as_str().map(String::from);
                    let sid = session.id.clone();
                    let loaded = self.threads.lock().unwrap().get(&sid).cloned();
                    let (active, recovered) = loaded
                        .as_ref()
                        .map(|thread| thread.recovered_turns(&items, &cwd, &meta))
                        .unwrap_or_else(|| (false, history_to_turns_with_ui(&items, &cwd, &meta)));
                    let turns = if p["excludeTurns"].as_bool().unwrap_or(false) {
                        vec![]
                    } else {
                        recovered
                    };
                    if loaded.is_none() {
                        let agent = self
                            .make_agent(session, items, &model, &cwd, sb, auto)
                            .await
                            .map_err(|e| (-32000, e.to_string()))?;
                        self.register(&sid, agent, &cwd, created);
                    }
                    let mut thread = self.thread_json(
                        &sid,
                        name.as_deref(),
                        "",
                        &cwd,
                        &model,
                        created,
                        now_ms(),
                        active,
                        turns,
                    );
                    thread["readOnly"] = json!(read_only);
                    Ok(
                        json!({"approvalPolicy": if auto { "never" } else { "on-request" }, "approvalsReviewer": "user", "cwd": cwd, "model": model,
                    "modelProvider": "openai", "sandbox": sandbox_policy(sb, &cwd), "thread": thread}),
                    )
                }
                "thread/list" => {
                    let limit = p["limit"].as_u64().unwrap_or(100).min(500) as usize;
                    let cwd_filter = p["cwd"].as_str().map(String::from);
                    let term = p["searchTerm"].as_str().map(|s| s.to_lowercase());
                    let source = if p["archived"].as_bool().unwrap_or(false) {
                        session::list_archived()
                    } else {
                        session::list()
                    };
                    let data: Vec<Value> = source
                        .into_iter()
                        .filter(|s| cwd_filter.as_ref().is_none_or(|c| &s.cwd == c))
                        .filter(|s| {
                            term.as_ref().is_none_or(|t| {
                                s.preview.to_lowercase().contains(t)
                                    || s.name.as_deref().unwrap_or("").to_lowercase().contains(t)
                                    || Session::open(Some(&s.id)).is_ok_and(|(_, items, _)| {
                                        items.iter().any(|i| {
                                            i["content"].as_array().is_some_and(|a| {
                                                a.iter().any(|c| {
                                                    c["text"].as_str().is_some_and(|text| {
                                                        text.to_lowercase().contains(t)
                                                    })
                                                })
                                            })
                                        })
                                    })
                            })
                        })
                        .map(|s| {
                            let active = self
                                .threads
                                .lock()
                                .unwrap()
                                .get(&s.id)
                                .is_some_and(|t| t.is_active());
                            let mut t = self.thread_json(
                                &s.id,
                                s.name.as_deref(),
                                &s.preview,
                                &s.cwd,
                                &s.model,
                                s.created_ms,
                                s.updated_ms,
                                active,
                                vec![],
                            );
                            t["readOnly"] = json!(s.read_only);
                            t["archived"] = json!(s.archived);
                            t
                        })
                        .filter(|t| p["mode"].as_str().is_none_or(|m| t["mode"] == m))
                        .take(limit)
                        .collect();
                    Ok(json!({"data": data, "nextCursor": null}))
                }
                "thread/read" => {
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let (session, items, meta) =
                        Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                    let cwd = meta["cwd"].as_str().unwrap_or(".");
                    let loaded = self.threads.lock().unwrap().get(&session.id).cloned();
                    let (active, recovered) = loaded
                        .as_ref()
                        .map(|thread| thread.recovered_turns(&items, cwd, &meta))
                        .unwrap_or_else(|| (false, history_to_turns_with_ui(&items, cwd, &meta)));
                    let turns = if p["includeTurns"].as_bool().unwrap_or(false) {
                        recovered
                    } else {
                        vec![]
                    };
                    let preview = history_to_turns(&items, cwd)
                        .first()
                        .and_then(|t| {
                            t["items"][0]["content"][0]["text"]
                                .as_str()
                                .map(|s| s.chars().take(60).collect::<String>())
                        })
                        .unwrap_or_default();
                    Ok(
                        json!({"thread": self.thread_json(&session.id, meta["name"].as_str(), &preview, cwd, meta["model"].as_str().unwrap_or(""), meta["created_ms"].as_u64().unwrap_or(0), now_ms(), active, turns)}),
                    )
                }
                "thread/name/set" => {
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let name = p["name"].as_str().ok_or_else(|| err("name"))?;
                    let (s, _, _) = Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                    s.try_set_name(name).map_err(|e| (-32000, e.to_string()))?;
                    self.notify(
                        "thread/name/updated",
                        json!({"threadId": id, "threadName": name}),
                    );
                    Ok(json!({}))
                }
                "thread/archive" | "thread/unarchive" | "thread/delete" => {
                    let requested_id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let (s, _, _) =
                        Session::open(Some(requested_id)).map_err(|e| (-32000, e.to_string()))?;
                    let id = s.id.clone();
                    let loaded = self.threads.lock().unwrap().get(&id).cloned();
                    let _start = match loaded.as_ref() {
                        Some(thread) => Some(thread.start_lock.lock().await),
                        None => None,
                    };
                    let running = loaded.as_ref().is_some_and(|thread| thread.is_active());
                    if running {
                        return Err((-32000, "Stop the running turn first".into()));
                    }
                    if method == "thread/delete" {
                        if s.is_read_only() {
                            return Err((-32000,"Imported sessions are read-only. Archive to hide this conversation.".into()));
                        }
                        let heartbeats = self.deletion_heartbeats(&id)?;
                        if !s.delete() {
                            return Err((-32000,"Conversation could not be deleted; its goal and heartbeat tasks were retained.".into()));
                        }
                        self.threads.lock().unwrap().remove(&id);
                        self.notify("thread/deleted", json!({"threadId":id}));
                        return self.clear_deleted_thread_dependencies(&id, &heartbeats);
                    }
                    match method {
                        "thread/archive" => s.archive().map_err(|e| (-32000, e.to_string()))?,
                        "thread/unarchive" => s.restore().map_err(|e| (-32000, e.to_string()))?,
                        _ => unreachable!(),
                    }
                    desktop::handle(
                        "desktop/threadMetadata/set",
                        &json!({"threadId":id,"archived":method=="thread/archive"}),
                    )
                    .unwrap()
                    .map_err(|e| (-32000, e))?;
                    self.notify("thread/archived", json!({"threadId":id}));
                    Ok(json!({}))
                }
                "thread/fork" => {
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let (_, mut items, meta) =
                        Session::open(Some(id)).map_err(|e| (-32000, e.to_string()))?;
                    if let Some(index) = p["beforeUserMessage"].as_u64() {
                        let end = visible_user_message_index(&items, index)
                            .ok_or_else(|| err("message index"))?;
                        items.truncate(end);
                    }
                    let cwd = meta["cwd"].as_str().unwrap_or(".");
                    let model = meta["model"].as_str().unwrap_or(&self.shared.default_model);
                    let session =
                        Session::create(cwd, model).map_err(|e| (-32000, e.to_string()))?;
                    for item in &items {
                        session.append(item);
                    }
                    for (position, snapshot) in
                        meta["_mcodex_ui_turns"].as_object().into_iter().flatten()
                    {
                        if let Ok(position) = position.parse::<usize>() {
                            if position < items.len() {
                                session
                                    .append_ui_snapshot(position, snapshot)
                                    .map_err(|error| (-32000, error.to_string()))?;
                            }
                        }
                    }
                    let sid = session.id.clone();
                    let old_meta =
                        desktop::handle("desktop/threadMetadata/read", &json!({"threadId":id}))
                            .unwrap()
                            .map_err(|e| (-32000, e))?;
                    let mode = old_meta["metadata"]["mode"].as_str().unwrap_or("codex");
                    let search = saved_search_preference(&old_meta["metadata"])
                        .map_err(|error| (-32000, error))?;
                    desktop::handle("desktop/threadMetadata/set", &json!({"threadId":sid,"mode":mode,"projectId":old_meta["metadata"]["projectId"],"webSearchMode":search.as_str()})).unwrap().map_err(|e| (-32000,e))?;
                    let (sb, auto) = self.params_policy(&p);
                    let agent = self
                        .make_agent(session, items.clone(), model, cwd, sb, auto)
                        .await
                        .map_err(|e| (-32000, e.to_string()))?;
                    self.register(&sid, agent, cwd, now_ms());
                    let thread = self.thread_json(
                        &sid,
                        None,
                        "",
                        cwd,
                        model,
                        now_ms(),
                        now_ms(),
                        false,
                        history_to_turns_with_ui(&items, cwd, &meta),
                    );
                    self.notify("thread/started", json!({"thread":thread}));
                    Ok(json!({"thread":thread,"cwd":cwd}))
                }
                "thread/webSearch/set" => {
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    let mut fields = json!({});
                    if let Some(mode) = p.get("mode") {
                        fields["webSearchMode"] = mode.clone();
                    }
                    if let Some(enabled) = p.get("enabled") {
                        fields["webSearch"] = enabled.clone();
                    }
                    let preferred = search_preference(&fields)
                        .map_err(|error| (-32602, error))?
                        .ok_or_else(|| err("Supply mode or enabled"))?;
                    let (session, _, _) =
                        Session::open(Some(id)).map_err(|error| (-32000, error.to_string()))?;
                    if session.is_read_only() {
                        return Err((
                            -32000,
                            "Fork this imported conversation before changing its search setting"
                                .into(),
                        ));
                    }
                    let metadata =
                        desktop::handle("desktop/threadMetadata/read", &json!({"threadId":id}))
                            .unwrap()
                            .map_err(|error| (-32000, error))?;
                    let sandbox = self
                        .threads
                        .lock()
                        .unwrap()
                        .get(id)
                        .map(|thread| *thread.sandbox.lock().unwrap())
                        .unwrap_or(self.shared.default_sandbox);
                    effective_search_mode(
                        preferred,
                        metadata["metadata"]["mode"].as_str().unwrap_or("codex"),
                        sandbox,
                    )
                    .map_err(|error| (-32000, error))?;
                    let updated = desktop::handle(
                        "desktop/threadMetadata/set",
                        &json!({"threadId":id,"webSearchMode":preferred.as_str()}),
                    )
                    .unwrap()
                    .map_err(|error| (-32000, error))?;
                    let thread = self.thread_json(id, None, "", "", "", 0, 0, false, vec![]);
                    let value = json!({"threadId":id,"webSearchMode":preferred.as_str(),"effectiveWebSearchMode":thread["effectiveWebSearchMode"],"webSearch":thread["webSearch"],"webSearchVersion":updated["metadata"]["webSearchVersion"]});
                    self.notify("thread/webSearch/updated", value.clone());
                    Ok(value)
                }
                "web/search/capabilities" => {
                    let sandbox = match p.get("sandboxMode") {
                        Some(value) => value
                            .as_str()
                            .and_then(sandbox_from)
                            .ok_or_else(|| err("Invalid sandboxMode"))?,
                        None => self.shared.default_sandbox,
                    };
                    let connected = auth::Auth::load()
                        .is_ok_and(|auth| !matches!(auth, auth::Auth::Unavailable));
                    let policy = search_policy_allowed().map_err(|error| (-32000, error))?;
                    let default = default_search_mode("codex", sandbox);
                    Ok(
                        json!({"available":connected,"permissionsVerified":false,"reason":if !connected{Some("Connect an account to use hosted web search".to_string())}else{default.as_ref().err().cloned()},"defaultModes":{"chat":"auto","work":"auto","codex":default.unwrap_or(SearchMode::Disabled).as_str()},"policy":{"allowed":policy},"modes":["auto","cached","live","required","disabled"]}),
                    )
                }
                "turn/start" => self.turn_start(p).await,
                "turn/interrupt" => {
                    if let Some(id) = p["threadId"].as_str() {
                        self.end_voice_for_thread(id).await?;
                    }
                    let id = p["threadId"].as_str().ok_or_else(|| err("threadId"))?;
                    if goals::get(id)
                        .map_err(|e| (-32000, e))?
                        .is_some_and(|g| g["status"] == "active" && g["inFlight"] == true)
                    {
                        if let Some(Ok(state)) = goals::handle(
                            "thread/goal/set",
                            &json!({"threadId":id,"status":"paused"}),
                        ) {
                            self.notify(
                                "thread/goal/updated",
                                json!({"threadId":id,"goal":state["goal"]}),
                            );
                        }
                    }
                    self.stop_thread(id)?;
                    Ok(json!({}))
                }
                "approval/list" => Ok(json!({"data": self.approvals(p["threadId"].as_str())})),
                "model/list" => {
                    let tbl = effective_config_table();
                    let cfg_model = tbl
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&self.shared.default_model)
                        .to_string();
                    let cfg_effort = tbl
                        .get("model_reasoning_effort")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                        .unwrap_or_else(||models::default_effort(&cfg_model));
                    let service_tier = tbl
                        .get("service_tier")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default")
                        .to_string();
                    let configured: Vec<String> = tbl
                        .get("models")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|m| m.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.models
                        .list(&p, configured, &cfg_model, &cfg_effort, &service_tier)
                        .await
                        .map_err(|e| (-32000, e))
                }
                "projects/list" => {
                    let gs = read_global_state();
                    let mut projects: Vec<Value> = Vec::new();
                    let mut seen_paths = std::collections::HashSet::new();
                    if let Some(lp) = gs.get("local-projects").and_then(|v| v.as_object()) {
                        let order: Vec<String> = gs
                            .get("project-order")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut keys: Vec<String> =
                            order.into_iter().filter(|k| lp.contains_key(k)).collect();
                        for k in lp.keys() {
                            if !keys.contains(k) {
                                keys.push(k.clone());
                            }
                        }
                        for k in keys {
                            if let Some(proj) = lp.get(&k) {
                                let root = proj["rootPaths"]
                                    .as_array()
                                    .and_then(|a| a.first())
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
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
                                if seen_paths.insert(path.clone())
                                    && std::path::Path::new(path).exists()
                                {
                                    let name = std::path::Path::new(path)
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| path.clone());
                                    projects.push(json!({"id": path, "name": name, "path": path, "exists": true}));
                                }
                            }
                        }
                    }
                    let selected_id = gs
                        .get("selected-project")
                        .and_then(|v| v["projectId"].as_str())
                        .map(String::from);
                    let assignments = gs
                        .get("thread-project-assignments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let pinned = gs
                        .get("pinned-thread-ids")
                        .cloned()
                        .unwrap_or_else(|| json!([]));
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
                    let out = std::process::Command::new("osascript")
                        .arg("-e")
                        .arg(&script)
                        .output()
                        .map_err(|e| (-32000, e.to_string()))?;
                    if !out.status.success() {
                        return Ok(json!({"cancelled": true, "path": null}));
                    }
                    let chosen = String::from_utf8_lossy(&out.stdout)
                        .trim()
                        .trim_end_matches('/')
                        .to_string();
                    let final_path = if chosen.is_empty() {
                        "/".to_string()
                    } else {
                        chosen
                    };
                    Ok(json!({"cancelled": false, "path": final_path}))
                }
                "projects/create" => {
                    let mode = p["mode"].as_str().unwrap_or("existing"); // "existing" | "new"
                    let init_git = p["initGit"].as_bool().unwrap_or(true);
                    let target_path = if mode == "new" {
                        let name = p["name"].as_str().unwrap_or("new-project").trim();
                        let parent =
                            p["parentDir"]
                                .as_str()
                                .map(PathBuf::from)
                                .unwrap_or_else(|| {
                                    std::env::var_os("HOME")
                                        .map(|h| PathBuf::from(h).join("Code"))
                                        .unwrap_or_else(|| self.shared.default_cwd.clone())
                                });
                        let full = parent.join(name);
                        std::fs::create_dir_all(&full)
                            .map_err(|e| (-32000, format!("Failed to create folder: {e}")))?;
                        if init_git && !full.join(".git").exists() {
                            let _ = std::process::Command::new("git")
                                .arg("init")
                                .current_dir(&full)
                                .output();
                        }
                        full
                    } else {
                        let raw = p["path"].as_str().ok_or_else(|| err("path"))?.trim();
                        let full = PathBuf::from(raw);
                        if !full.exists() {
                            std::fs::create_dir_all(&full)
                                .map_err(|e| (-32000, format!("Failed to create folder: {e}")))?;
                        }
                        if init_git
                            && p["initGit"].as_bool() == Some(true)
                            && !full.join(".git").exists()
                        {
                            let _ = std::process::Command::new("git")
                                .arg("init")
                                .current_dir(&full)
                                .output();
                        }
                        full
                    };
                    let path_str = target_path.display().to_string();
                    let proj_name = p["name"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| s.trim().to_string())
                        .or_else(|| {
                            target_path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                        })
                        .unwrap_or_else(|| "Project".to_string());

                    let mut gs = read_global_state();
                    if !gs["local-projects"].is_object() {
                        gs["local-projects"] = json!({});
                    }
                    // Check if project already exists for this path
                    let mut proj_id = format!("proj-{:x}", now_ms());
                    if let Some(lp) = gs["local-projects"].as_object() {
                        for (k, v) in lp {
                            if v["rootPaths"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|x| x.as_str())
                                == Some(&path_str)
                            {
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
                    let mut order: Vec<Value> =
                        gs["project-order"].as_array().cloned().unwrap_or_default();
                    if !order.iter().any(|x| x.as_str() == Some(&proj_id)) {
                        order.insert(0, json!(proj_id));
                        gs["project-order"] = json!(order);
                    }
                    gs["selected-project"] = json!({"type": "local", "projectId": proj_id});
                    write_global_state(&gs);

                    // Also mark trusted in config.toml
                    let mut tbl = read_codex_config_table();
                    let projs = tbl
                        .entry("projects")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let Some(pt) = projs.as_table_mut() {
                        let mut entry = toml::Table::new();
                        entry.insert("trust_level".into(), toml::Value::String("trusted".into()));
                        pt.insert(path_str.clone(), toml::Value::Table(entry));
                    }
                    write_codex_config_table(&tbl).map_err(|e| (-32000, e.to_string()))?;

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
                    let _ = std::process::Command::new("/usr/bin/open")
                        .args(["-a", app_name, path])
                        .spawn();
                    Ok(json!({"ok": true}))
                }
                "settings/read" => {
                    let tbl = effective_config_table();
                    let raw_toml = std::fs::read_to_string(codex_config_path()).unwrap_or_default();
                    let global_agents_md =
                        std::fs::read_to_string(config::home().join("AGENTS.md"))
                            .unwrap_or_default();
                    let cwd = p["cwd"]
                        .as_str()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.shared.default_cwd.clone());
                    let workspace_agents_md =
                        std::fs::read_to_string(cwd.join("AGENTS.md")).unwrap_or_default();
                    let json_cfg: Value = serde_json::to_value(&tbl).unwrap_or_else(|_| json!({}));
                    Ok(json!({
                        "config": json_cfg,
                        "rawToml": raw_toml, "rawConfigToml": raw_toml,
                        "sandboxMode": tbl.get("sandbox_mode").and_then(|v| v.as_str()).unwrap_or("workspace-write"),
                        "configPath": codex_config_path().display().to_string(),
                        "globalAgentsMd": global_agents_md,
                        "workspaceAgentsMd": workspace_agents_md, "projectAgentsMd": workspace_agents_md,
                    }))
                }
                "settings/write" => {
                    if let Some(model)=p["model"].as_str(){if let Some(metadata)=self.models.metadata(model).await {if let Some(effort)=p["reasoningEffort"].as_str().filter(|effort|*effort!="auto"){if metadata["reasoningMetadataKnown"]==true&&!metadata["supportedReasoningEfforts"].as_array().is_some_and(|options|options.iter().any(|option|option["reasoningEffort"]==effort)){return Err((-32602,"Reasoning effort is not supported by the selected model".into()));}}}}
                    let mut tbl = read_codex_config_table();
                    if p["inheritModelDefaults"]==true {tbl=std::fs::read_to_string(codex_config_path()).ok().and_then(|text|text.parse().ok()).unwrap_or_default();for key in ["model","model_reasoning_effort","service_tier"]{tbl.remove(key);}}
                    if let Some(value)=p.get("reasoningEffort"){if value.as_str().is_none_or(|effort|effort!="auto"&&!models::EFFORTS.contains(&effort)){return Err((-32602,"Invalid reasoning effort".into()));}}
                    if let Some(value)=p.get("serviceTier"){if value.as_str().is_none_or(|tier|!["auto","default","fast","priority","flex","ultrafast"].contains(&tier)){return Err((-32602,"Invalid service tier".into()));}}
                    if let Some(m) = p["model"].as_str() {
                        tbl.insert("model".into(), toml::Value::String(m.into()));
                    }
                    if let Some(e) = p["reasoningEffort"].as_str() {
                        tbl.insert(
                            "model_reasoning_effort".into(),
                            toml::Value::String(e.into()),
                        );
                    }
                    if let Some(st) = p["serviceTier"].as_str() {
                        if let Some(tier)=models::normalize_tier(st){tbl.insert("service_tier".into(), toml::Value::String(tier));}else{tbl.insert("service_tier".into(),toml::Value::String("default".into()));}
                    }
                    if let Some(sb) = p["sandboxMode"].as_str() {
                        tbl.insert("sandbox_mode".into(), toml::Value::String(sb.into()));
                    }
                    if let Some(ap) = p["approvalPolicy"].as_str() {
                        tbl.insert("approval_policy".into(), toml::Value::String(ap.into()));
                    }
                    if let Some(desktop_updates) = p["desktop"].as_object() {
                        let desk = tbl
                            .entry("desktop")
                            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
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
                    if let Some(raw) = p["rawToml"].as_str().or(p["rawConfigToml"].as_str()) {
                        let parsed = raw
                            .parse::<toml::Table>()
                            .map_err(|e| (-32000, format!("Invalid TOML: {e}")))?;
                        tbl = parsed;
                    }
                    write_codex_config_table(&tbl).map_err(|e| (-32000, e.to_string()))?;
                    if let Some(ga) = p["globalAgentsMd"].as_str() {
                        let _ = std::fs::write(config::home().join("AGENTS.md"), ga);
                    }
                    if let Some(wa) = p["workspaceAgentsMd"]
                        .as_str()
                        .or(p["projectAgentsMd"].as_str())
                    {
                        let cwd = p["cwd"]
                            .as_str()
                            .map(PathBuf::from)
                            .unwrap_or_else(|| self.shared.default_cwd.clone());
                        let _ = std::fs::write(cwd.join("AGENTS.md"), wa);
                    }
                    Ok(json!({"ok": true}))
                }
                "mcp/addServer" => {
                    let name = p["name"].as_str().ok_or_else(|| err("name"))?.trim();
                    let command = p["command"].as_str().ok_or_else(|| err("command"))?.trim();
                    let args: Vec<toml::Value> = p["args"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(|s| toml::Value::String(s.into())))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mut tbl = read_codex_config_table();
                    let mcp_tbl = tbl
                        .entry("mcp_servers")
                        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
                    if let Some(mt) = mcp_tbl.as_table_mut() {
                        let mut srv = toml::Table::new();
                        srv.insert("type".into(), toml::Value::String("stdio".into()));
                        srv.insert("command".into(), toml::Value::String(command.into()));
                        srv.insert("args".into(), toml::Value::Array(args));
                        mt.insert(name.into(), toml::Value::Table(srv));
                    }
                    write_codex_config_table(&tbl).map_err(|e| (-32000, e.to_string()))?;
                    Ok(json!({"ok": true}))
                }
                "mcp/removeServer" => {
                    let name = p["name"].as_str().ok_or_else(|| err("name"))?;
                    let mut tbl = read_codex_config_table();
                    if let Some(mt) = tbl.get_mut("mcp_servers").and_then(|v| v.as_table_mut()) {
                        mt.remove(name);
                    }
                    write_codex_config_table(&tbl).map_err(|e| (-32000, e.to_string()))?;
                    Ok(json!({"ok": true}))
                }
                "clipboard/write" => {
                    let text = p["text"].as_str().ok_or_else(|| err("text"))?.to_string();
                    if text.len() > 4_000_000 {
                        return Err(err("Clipboard text exceeds 4 MB"));
                    }
                    #[cfg(target_os = "macos")]
                    {
                        tokio::task::spawn_blocking(move || -> Result<()> {
                            use std::io::Write;
                            let mut process = std::process::Command::new("/usr/bin/pbcopy")
                                .stdin(std::process::Stdio::piped())
                                .spawn()?;
                            process
                                .stdin
                                .take()
                                .ok_or_else(|| anyhow::anyhow!("Clipboard input unavailable"))?
                                .write_all(text.as_bytes())?;
                            if !process.wait()?.success() {
                                anyhow::bail!("Could not copy to the clipboard");
                            }
                            Ok(())
                        })
                        .await
                        .map_err(|e| (-32000, e.to_string()))?
                        .map_err(|e| (-32000, e.to_string()))?;
                        Ok(json!({"ok":true}))
                    }
                    #[cfg(not(target_os = "macos"))]
                    {
                        let _ = text;
                        Err((-32000, "Native clipboard is only available on macOS".into()))
                    }
                }
                "config/read" => {
                    let tbl = effective_config_table();
                    let cfg_val: Value = serde_json::to_value(&tbl)
                        .unwrap_or_else(|_| json!({"model": self.shared.default_model}));
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
                    let mut data: Vec<Value> = plugins
                        .iter()
                        .map(|pl| {
                            json!({
                                "name": pl.name,
                                "description": pl.dir.display().to_string(),
                                "skills": pl.tools.len(),
                                "enabled": true,
                                "source": "local",
                            })
                        })
                        .collect();
                    let tbl = read_codex_config_table();
                    if let Some(pl_tbl) = tbl.get("plugins").and_then(|v| v.as_table()) {
                        for (k, v) in pl_tbl {
                            let enabled =
                                v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true);
                            data.push(json!({
                                "name": k,
                                "description": "Bundled Codex plugin",
                                "skills": 1,
                                "enabled": enabled,
                                "source": "codex-plugin",
                            }));
                        }
                    }
                    if let Some(sk_arr) = tbl
                        .get("skills")
                        .and_then(|v| v.get("config"))
                        .and_then(|v| v.as_array())
                    {
                        for sk in sk_arr {
                            if let Some(path) = sk.get("path").and_then(|v| v.as_str()) {
                                let enabled =
                                    sk.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                                let name = std::path::Path::new(path)
                                    .parent()
                                    .and_then(|p| p.file_name())
                                    .map(|n| n.to_string_lossy().to_string())
                                    .unwrap_or_else(|| path.to_string());
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
                "fs/readDirectory" => {
                    let cwd = p["cwd"]
                        .as_str()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.shared.default_cwd.clone());
                    let query = p["query"].as_str().unwrap_or("").to_lowercase();
                    Ok(list_workspace_files(&cwd, &query))
                }
                "fs/readFile" => {
                    let cwd = p["cwd"]
                        .as_str()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.shared.default_cwd.clone());
                    let rel = p["path"].as_str().ok_or_else(|| err("path"))?;
                    let full = if std::path::Path::new(rel).is_absolute() {
                        PathBuf::from(rel)
                    } else {
                        cwd.join(rel)
                    };
                    let root = cwd.canonicalize().map_err(|e| (-32000, e.to_string()))?;
                    let full = full.canonicalize().map_err(|e| (-32000, e.to_string()))?;
                    if !full.starts_with(&root) {
                        return Err(err("File is outside the selected workspace"));
                    }
                    if full.metadata().map_err(|e| (-32000, e.to_string()))?.len() > 1_000_000 {
                        return Err(err("File exceeds 1 MB"));
                    }
                    let content =
                        std::fs::read_to_string(&full).map_err(|e| (-32000, e.to_string()))?;
                    let truncated: String = content.chars().take(32_000).collect();
                    Ok(json!({"path": rel, "content": truncated}))
                }
                "command/exec" => {
                    let _gate = self.workspace_gate.lock().await;
                    let cwd = p["cwd"]
                        .as_str()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| self.shared.default_cwd.clone());
                    let cmd = p["command"].as_str().ok_or_else(|| err("command"))?;
                    let (sandbox, _) = self.params_policy(&p);
                    let ctx = ToolCtx {
                        cwd: cwd.clone(),
                        sandbox,
                        auto_approve: true,
                        mcp: self.shared.mcp.clone(),
                        plugins: vec![],
                        session_allow: Default::default(),
                    };
                    let output = crate::tools::exec(
                        &["/bin/sh".into(), "-lc".into(), cmd.into()],
                        &cwd,
                        std::time::Duration::from_secs(20),
                        &ctx,
                    )
                    .await
                    .map_err(|e| (-32000, e.to_string()))?;
                    let exit = output
                        .lines()
                        .next()
                        .and_then(|l| l.strip_prefix("exit code: "))
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(-1);
                    Ok(json!({"exitCode":exit,"output":output}))
                }
                _ => Err((-32601, format!("method not found: {method}"))),
            }
        })
    }

    async fn turn_start(self: &Arc<Self>, p: Value) -> RpcResult {
        let _workspace_gate = self.workspace_gate.lock().await;
        let bad = |m: &str| (-32602, m.to_string());
        let requested_web_search = search_preference(&p).map_err(|error| (-32602, error))?;
        let explicit_search_version = p
            .get("webSearchVersion")
            .map(|value| {
                value
                    .as_u64()
                    .ok_or_else(|| bad("webSearchVersion must be an integer"))
            })
            .transpose()?;
        let tid = p["threadId"]
            .as_str()
            .ok_or_else(|| bad("threadId"))?
            .to_string();
        let t = self
            .threads
            .lock()
            .unwrap()
            .get(&tid)
            .cloned()
            .ok_or_else(|| bad("thread not loaded; call thread/start or thread/resume"))?;
        let _start = t.start_lock.lock().await;
        if t.is_active() {
            return Err((-32000, "a turn is already running".into()));
        }
        if p["goalContinuation"].as_bool() == Some(true)
            && !t.queued_inputs.lock().unwrap().is_empty()
        {
            return Err((-32000, "A user message is queued".into()));
        }
        let chosen_model=match p["model"].as_str(){Some(model)=>model.to_owned(),None=>t.agent.lock().await.model.clone()};
        let model_metadata=self.models.metadata(&chosen_model).await;
        if let Some(effort)=p["reasoningEffort"].as_str().or_else(||p["effort"].as_str()) {
            if effort!="auto"&&!models::EFFORTS.contains(&effort){return Err(bad("Invalid reasoning effort"));}
            if let Some(metadata)=model_metadata.as_ref().filter(|metadata|metadata["reasoningMetadataKnown"]==true) {
                if effort!="auto"&&!metadata["supportedReasoningEfforts"].as_array().is_some_and(|options|options.iter().any(|option|option["reasoningEffort"]==effort)){return Err(bad("Reasoning effort is not supported by the selected model"));}
            }
        }
        let initial_meta = desktop::handle("desktop/threadMetadata/read", &json!({"threadId":tid}))
            .unwrap()
            .map_err(|error| (-32000, error))?;
        let search_version = explicit_search_version.unwrap_or_else(|| {
            initial_meta["metadata"]["webSearchVersion"]
                .as_u64()
                .unwrap_or(0)
        });
        let mut text = String::new();
        let mut display_text = String::new();
        let mut imported_attachments = Vec::new();
        let mut materialized_bytes = 0usize;
        let mut content = Vec::new();
        for i in p["input"].as_array().cloned().unwrap_or_default() {
            match i["type"].as_str() {
                Some("text") => {
                    let value = i["text"].as_str().unwrap_or("");
                    text.push_str(value);
                    display_text.push_str(value);
                }
                Some("mention") => {
                    let path = i["path"].as_str().ok_or_else(|| bad("mention path"))?;
                    let root = PathBuf::from(&t.cwd)
                        .canonicalize()
                        .map_err(|e| (-32000, e.to_string()))?;
                    let full = root
                        .join(path)
                        .canonicalize()
                        .map_err(|e| (-32000, e.to_string()))?;
                    if !full.starts_with(&root) {
                        return Err(bad("Mention is outside the selected workspace"));
                    }
                    if full.metadata().map_err(|e| (-32000, e.to_string()))?.len() > 200_000 {
                        return Err(bad("Mention exceeds 200 KB"));
                    }
                    let body =
                        std::fs::read_to_string(full).map_err(|e| (-32000, e.to_string()))?;
                    text.push_str(&format!(
                        "\n\n<workspace_file path={path:?}>\n{body}\n</workspace_file>"
                    ));
                }
                Some("skill") => text.push_str(&format!(
                    "\n[{}: {}]",
                    i["name"].as_str().unwrap_or(""),
                    i["path"].as_str().unwrap_or("")
                )),
                Some("image") => {
                    let url = i["imageUrl"].as_str().ok_or_else(|| bad("imageUrl"))?;
                    if url.len() > 6_000_000
                        || ![
                            "data:image/png;base64,",
                            "data:image/jpeg;base64,",
                            "data:image/webp;base64,",
                        ]
                        .iter()
                        .any(|p| url.starts_with(p))
                    {
                        return Err(bad("Unsupported image or image exceeds 4 MB"));
                    }
                    content.push(json!({"type":"input_image","image_url":url}));
                }
                Some("attachment") => {
                    if let Some(id) = i["attachmentId"].as_str() {
                        if imported_attachments.len() >= 20 {
                            return Err(bad("A message can include up to 20 stored attachments"));
                        }
                        let id = id.to_string();
                        let (mut metadata, parts) = tokio::task::spawn_blocking(move || {
                            Ok::<_, String>((
                                attachments::read_metadata(&id)?,
                                attachments::model_content(&id)?,
                            ))
                        })
                        .await
                        .map_err(|e| (-32000, e.to_string()))?
                        .map_err(|e| (-32602, e))?;
                        for part in parts {
                            if part["type"] == "input_image" {
                                metadata["imageIndex"] = json!(content
                                    .iter()
                                    .filter(|part| part["type"] == "input_image")
                                    .count());
                            }
                            materialized_bytes = materialized_bytes.saturating_add(
                                part["text"]
                                    .as_str()
                                    .map(str::len)
                                    .or_else(|| part["image_url"].as_str().map(str::len))
                                    .unwrap_or(0),
                            );
                            if materialized_bytes > 32 * 1024 * 1024 {
                                return Err(bad("Combined attachment content exceeds 32 MiB; send fewer files in one message"));
                            }
                            content.push(part);
                        }
                        imported_attachments.push(metadata);
                        continue;
                    }
                    let name = i["name"].as_str().unwrap_or("file");
                    let body = i["text"]
                        .as_str()
                        .ok_or_else(|| bad("Text attachment requires text"))?;
                    if body.len() > 200_000 {
                        return Err(bad("Text attachment exceeds 200 KB"));
                    }
                    text.push_str(&format!(
                        "\n\n<attachment name={name:?}>\n{body}\n</attachment>"
                    ));
                }
                Some("localImage") => return Err(bad("Use image input with imageUrl data URL")),
                _ => {}
            }
        }
        if !text.is_empty() {
            content.insert(0, json!({"type":"input_text","text":text}));
        }
        if content.is_empty() {
            return Err(bad("input is empty"));
        }
        let mut presentation = json!({"text":if imported_attachments.is_empty(){&text}else{&display_text},"attachments":imported_attachments});
        let history_len;
        {
            let mut a = t.agent.lock().await;
            history_len = a.history.len();
            a.goal_thread_id = if p["scheduledRunId"].is_null()
                && goals::get(&tid)
                    .map_err(|e| (-32000, e))?
                    .is_some_and(|g| g["status"] == "active")
            {
                Some(tid.clone())
            } else {
                None
            };
            a.scheduled_run_id = p["scheduledRunId"].as_str().map(String::from);
            a.client.auth = auth::Auth::load().unwrap_or(auth::Auth::Unavailable);
            if a.session.is_read_only() {
                return Err((
                    -32000,
                    "Fork this imported conversation before continuing.".into(),
                ));
            }
            let meta = desktop::handle("desktop/threadMetadata/read", &json!({"threadId":tid}))
                .unwrap()
                .map_err(|e| (-32000, e))?;
            a.mode = meta["metadata"]["mode"].as_str().unwrap_or("codex").into();
            if a.mode == "chat" {
                a.instructions = "You are a helpful assistant. Answer clearly. Use Markdown where useful. You have no access to the user's computer in Chat mode.".into();
            } else {
                a.instructions =
                    crate::build_instructions(&self.shared.cfg, &a.ctx.plugins, &a.ctx.cwd);
                if a.mode == "work" {
                    a.instructions.push_str("\nHelp the user research, write and complete tasks. Use local tools when needed. Do not claim cloud integrations or services are connected unless a tool provides them.");
                }
            }
            if let Some(instructions) = desktop::handle("desktop/settings/read", &json!({}))
                .and_then(|r| r.ok())
                .and_then(|r| {
                    r["settings"]["preferences"]["customInstructions"]
                        .as_str()
                        .map(String::from)
                })
            {
                a.instructions.push_str(&format!("\n\n{instructions}"));
            }
            if let Some(project_id) = meta["metadata"]["projectId"].as_str() {
                if let Some(Ok(project)) =
                    desktop::handle("desktop/project/read", &json!({"id":project_id}))
                {
                    if let Some(instructions) = project["project"]["instructions"].as_str() {
                        a.instructions
                            .push_str(&format!("\n\n# Project instructions\n{instructions}"));
                    }
                }
            }
            let table = effective_config_table();
            a.client.service_tier = table
                .get("service_tier")
                .and_then(|v| v.as_str())
                .and_then(models::normalize_tier);
            if let Some(m) = p["model"].as_str() {
                a.model = m.into();
            }
            if let Some(tier)=p.get("serviceTier"){if !tier.is_null()&&!tier.is_string(){return Err((-32602,"serviceTier must be a string or null".into()));}a.client.service_tier=tier.as_str().and_then(models::normalize_tier);}
            if let Some(eff) = p["reasoningEffort"]
                .as_str()
                .or_else(|| p["effort"].as_str())
            {
                a.reasoning_effort = eff.into();
            }
            if let Some(metadata)=model_metadata.as_ref(){a.reasoning_effort=models::resolve_effort(metadata,&a.reasoning_effort);}
            let (sb, auto) = self.params_policy(&p);
            if p.get("sandboxPolicy").is_some() {
                a.ctx.sandbox = sb;
            }
            if p["approvalPolicy"].is_string() {
                a.ctx.auto_approve = auto;
            }
            *t.sandbox.lock().unwrap() = a.ctx.sandbox;
            let preferred = match requested_web_search {
                Some(mode) => mode,
                None => {
                    saved_search_preference(&meta["metadata"]).map_err(|error| (-32000, error))?
                }
            };
            let effective = effective_search_mode(preferred, &a.mode, a.ctx.sandbox)
                .map_err(|error| (-32000, error))?;
            a.web_search = effective;
            presentation["webSearchMode"] = json!(preferred.as_str());
            presentation["effectiveWebSearchMode"] = json!(effective.as_str());
            presentation["webSearch"] = json!(effective.enabled());
            if requested_web_search.is_some() {
                let updated=desktop::handle("desktop/threadMetadata/set",&json!({"threadId":tid,"webSearchMode":preferred.as_str(),"expectedWebSearchVersion":search_version})).unwrap().map_err(|error|(-32000,error))?;
                if updated.get("conflict").is_none() {
                    self.notify("thread/webSearch/updated",json!({"threadId":tid,"webSearchMode":preferred.as_str(),"effectiveWebSearchMode":effective.as_str(),"webSearch":effective.enabled(),"webSearchVersion":updated["metadata"]["webSearchVersion"]}));
                }
            }
        }
        if p["scheduledRunId"].is_null()
            && p["goalContinuation"] != true
            && goals::get(&tid)
                .map_err(|e| (-32000, e))?
                .is_some_and(|g| g["status"] == "active" && g["suppressed"] == true)
        {
            if let Some(Ok(state)) = goals::handle(
                "thread/goal/set",
                &json!({"threadId":tid,"status":"active"}),
            ) {
                self.notify(
                    "thread/goal/updated",
                    json!({"threadId":tid,"goal":state["goal"]}),
                );
            }
        }
        let turn_id = self.new_id("turn");
        let goal_before_turn = goals::get(&tid).map_err(|e| (-32000, e))?;
        let internal = internal_goal_continuation(&p, goal_before_turn.as_ref());
        if p["scheduledRunId"].is_null()
            && goal_before_turn
                .as_ref()
                .is_some_and(|g| g["status"] == "active")
        {
            let state = goals::begin_turn(&tid, &turn_id).map_err(|e| (-32000, e))?;
            if let Some(goal) = state {
                self.notify("thread/goal/updated", json!({"threadId":tid,"goal":goal}));
            }
        }
        let scheduled_run_id = p["scheduledRunId"].clone();
        let started = now_ms();
        *t.turn_id.lock().unwrap() = Some(turn_id.clone());
        t.interrupted.store(false, Ordering::SeqCst);
        let turn = |status: &str, ended: Option<u64>| {
            json!({"id": turn_id, "items": [], "scheduledRunId":scheduled_run_id, "status": status, "startedAt": started / 1000,
            "completedAt": ended.map(|e| e / 1000), "durationMs": ended.map(|e| e - started)})
        };

        *t.live.lock().unwrap() = Some(LiveTurn::new(history_len, turn("inProgress", None)));
        self.notify(
            "turn/started",
            json!({"threadId": tid, "turn": turn("inProgress", None)}),
        );
        self.notify(
            "thread/status/changed",
            json!({"threadId": tid, "status": {"type": "active", "activeFlags": []}}),
        );
        let mut visible_attachments = content
            .iter()
            .filter(|c| c["type"] == "input_image")
            .cloned()
            .collect::<Vec<_>>();
        visible_attachments.extend(imported_attachments);
        let user_item = json!({"type": "userMessage", "id": self.new_id("item"), "internal":internal, "content": [{"type": "text", "text": presentation["text"].as_str().unwrap_or(&text), "text_elements": []}], "attachments":visible_attachments,"webSearchMode":presentation["webSearchMode"],"effectiveWebSearchMode":presentation["effectiveWebSearchMode"],"webSearch":presentation["webSearch"]});
        self.notify(
            "item/started",
            json!({"item": user_item, "startedAtMs": started, "threadId": tid, "turnId": turn_id}),
        );
        self.notify("item/completed", json!({"item": user_item, "completedAtMs": started, "threadId": tid, "turnId": turn_id}));

        let (tx, mut rx) = unbounded_channel::<Event>();
        let agent = t.agent.clone();
        let runner = tokio::spawn(async move {
            agent
                .lock()
                .await
                .turn_content_presented(content, &tx, internal, Some(presentation))
                .await
        });
        *t.runner.lock().unwrap() = Some(runner);

        let srv = self.clone();
        let scheduled_run_id2 = scheduled_run_id.clone();
        let (tid2, turn_id2, cwd, interrupted) = (
            tid.clone(),
            turn_id.clone(),
            t.cwd.clone(),
            t.interrupted.clone(),
        );
        tokio::spawn(async move {
            let mut open_msg: Option<(String, String)> = None;
            let mut open_reasoning: Option<(String, String, u64)> = None;
            let mut items: HashMap<String, (Value, u64)> = HashMap::new();
            let mut messages: HashMap<String, (String, String, u64)> = HashMap::new();
            let mut completed_messages = std::collections::HashSet::new();
            let mut searches: HashMap<String, (Value, u64)> = HashMap::new();
            let mut error: Option<String> = None;
            let mut tokens_used = 0u64;
            let mut usage_known = false;
            let mut had_work_tools = false;
            let mut task_report = None;
            let ids = |m: &str| json!({"threadId": tid2, "turnId": turn_id2, "itemId": m});
            let close_msg =
                |srv: &Server,
                 open: &mut Option<(String, String)>,
                 known: &mut HashMap<String, (String, String, u64)>| {
                    if let Some((id, text)) = open.take() {
                        if let Some(message) = known.values_mut().find(|message| message.0 == id) {
                            message.1 = text;
                            return;
                        }
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
                    Event::AssistantMessage(raw) => {
                        let key = raw["_mcodex_item_key"]
                            .as_str()
                            .or(raw["id"].as_str())
                            .unwrap_or("anonymous")
                            .to_string();
                        let activate = raw["_mcodex_activate"] == true;
                        if let Some((id, text)) = open_msg.as_ref() {
                            if let Some(state) = messages.values_mut().find(|state| state.0 == *id)
                            {
                                state.1 = text.clone();
                            }
                        }
                        if !messages.contains_key(&key) {
                            let known_open = open_msg.as_ref().is_some_and(|(id, _)| {
                                messages.values().any(|state| state.0 == *id)
                            });
                            let (id, text) = if !activate && !known_open {
                                open_msg
                                    .take()
                                    .unwrap_or_else(|| (srv.new_id("msg"), String::new()))
                            } else {
                                (srv.new_id("msg"), String::new())
                            };
                            let now = now_ms();
                            messages.insert(key.clone(), (id.clone(), text.clone(), now));
                            srv.notify("item/started",json!({"threadId":tid2,"turnId":turn_id2,"startedAtMs":now,"item":{"type":"agentMessage","id":id,"text":text,"status":"inProgress"}}));
                        }
                        let state = messages.get_mut(&key).unwrap();
                        if activate {
                            open_msg = Some((state.0.clone(), state.1.clone()));
                            continue;
                        }
                        match crate::web_search::project_message(&raw) {
                            Ok(mut item) => {
                                state.1 = item["text"].as_str().unwrap_or("").into();
                                item["id"] = json!(state.0);
                                item["type"] = json!("agentMessage");
                                item["status"] =
                                    raw.get("status").cloned().unwrap_or(json!("completed"));
                                completed_messages.insert(key);
                                srv.notify("item/completed",json!({"item":item,"threadId":tid2,"turnId":turn_id2,"startedAtMs":state.2,"completedAtMs":now_ms()}));
                                if open_msg.as_ref().is_some_and(|(id, _)| *id == state.0) {
                                    open_msg = None;
                                }
                            }
                            Err(message) => error = Some(message),
                        }
                    }
                    Event::WebSearch(raw) => {
                        let key = raw["_mcodex_item_key"]
                            .as_str()
                            .or(raw["id"].as_str())
                            .unwrap_or("anonymous")
                            .to_string();
                        match crate::web_search::project_call(&raw) {
                            Ok(mut item) => {
                                let previous = searches.get(&key);
                                if previous.is_some_and(|(old, _)| old["status"] == "completed")
                                    && matches!(
                                        item["status"].as_str(),
                                        Some("in_progress" | "searching")
                                    )
                                {
                                    continue;
                                }
                                let (id, started_at) = previous
                                    .map(|(item, start)| {
                                        (item["id"].as_str().unwrap().to_string(), *start)
                                    })
                                    .unwrap_or_else(|| (srv.new_id("web"), now_ms()));
                                item["id"] = json!(id);
                                let active = matches!(
                                    item["status"].as_str(),
                                    Some("in_progress" | "searching")
                                );
                                if item["status"] == "completed" {
                                    had_work_tools = true;
                                }
                                let mut params = json!({"item":item,"threadId":tid2,"turnId":turn_id2,"startedAtMs":started_at});
                                if !active {
                                    params["completedAtMs"] = json!(now_ms());
                                }
                                srv.notify(
                                    if active {
                                        "item/started"
                                    } else {
                                        "item/completed"
                                    },
                                    params,
                                );
                                searches.insert(key, (item, started_at));
                            }
                            Err(message) => error = Some(message),
                        }
                    }
                    Event::ReasoningStart { id } => {
                        close_msg(&srv, &mut open_msg, &mut messages);
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
                        had_work_tools = true;
                        close_reasoning(&srv, &mut open_reasoning);
                        close_msg(&srv, &mut open_msg, &mut messages);
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
                    Event::Approval {
                        id,
                        name,
                        what,
                        reply,
                    } => {
                        let (method, params) = match name.as_str() {
                            "apply_patch" => (
                                "item/fileChange/requestApproval",
                                json!({"itemId": id, "threadId": tid2, "turnId": turn_id2, "reason": null, "startedAtMs": now_ms()}),
                            ),
                            _ => (
                                "item/commandExecution/requestApproval",
                                json!({"itemId": id, "threadId": tid2, "turnId": turn_id2, "command": what, "cwd": cwd, "startedAtMs": now_ms()}),
                            ),
                        };
                        let srv2 = srv.clone();
                        let approval_interrupted = interrupted.clone();
                        tokio::spawn(async move {
                            let resp = srv2.request(method, params, approval_interrupted).await;
                            let v = match resp["decision"].as_str() {
                                Some("accept") => Verdict::Yes,
                                Some("acceptForSession") => Verdict::Always,
                                _ => Verdict::No,
                            };
                            let _ = reply.send(v);
                        });
                    }
                    Event::Usage { tokens } => {
                        if let Some(tokens) = tokens {
                            usage_known = true;
                            tokens_used = tokens_used.saturating_add(tokens);
                        }
                    }
                    Event::GoalState(state) => {
                        if state["threadId"] == tid2 {
                            if let Ok(Some(goal)) = goals::get(&tid2) {
                                srv.notify(
                                    "thread/goal/updated",
                                    json!({"threadId":tid2,"goal":goal}),
                                );
                            }
                        }
                    }
                    Event::TaskReport(report) => task_report = Some(report),
                    Event::Done => {}
                    Event::Error(e) => error = Some(e),
                }
            }
            // Channel closed: the turn finished, failed, or was aborted.
            if let Some((id, text)) = open_msg.as_ref() {
                if let Some(state) = messages.values_mut().find(|state| state.0 == *id) {
                    state.1 = text.clone();
                }
            }
            close_reasoning(&srv, &mut open_reasoning);
            close_msg(&srv, &mut open_msg, &mut messages);
            for (key, (id, text, started_at)) in messages {
                if !completed_messages.contains(&key) {
                    let status = if interrupted.load(Ordering::SeqCst) {
                        "interrupted"
                    } else if error.is_some() {
                        "failed"
                    } else {
                        "incomplete"
                    };
                    srv.notify("item/completed",json!({"item":{"type":"agentMessage","id":id,"text":text,"status":status},"threadId":tid2,"turnId":turn_id2,"startedAtMs":started_at,"completedAtMs":now_ms()}));
                }
            }
            for (mut item, started_at) in searches.into_values() {
                if matches!(item["status"].as_str(), Some("in_progress" | "searching")) {
                    item["status"] = json!(if interrupted.load(Ordering::SeqCst) {
                        "interrupted"
                    } else if error.is_some() {
                        "failed"
                    } else {
                        "incomplete"
                    });
                    srv.notify("item/completed",json!({"item":item,"threadId":tid2,"turnId":turn_id2,"startedAtMs":started_at,"completedAtMs":now_ms()}));
                }
            }
            for (mut item, started_at) in items.into_values() {
                item["status"] = json!(if interrupted.load(Ordering::SeqCst) {
                    "interrupted"
                } else {
                    "failed"
                });
                item["durationMs"] = json!(now_ms().saturating_sub(started_at));
                srv.notify("item/completed",json!({"item":item,"threadId":tid2,"turnId":turn_id2,"startedAtMs":started_at,"completedAtMs":now_ms()}));
            }
            let ended = now_ms();
            let was_interrupted = interrupted.load(Ordering::SeqCst);
            if was_interrupted {
                srv.agent_fix(&tid2).await;
            }
            let mut t = json!({"id": turn_id2, "items": [], "scheduledRunId":scheduled_run_id2, "startedAt": started / 1000, "completedAt": ended / 1000, "durationMs": ended - started});
            if was_interrupted {
                t["status"] = json!("interrupted");
            } else if let Some(e) = error.as_ref() {
                srv.notify("error", json!({"error": {"message": e}, "threadId": tid2, "turnId": turn_id2, "willRetry": false}));
                t["status"] = json!("failed");
                t["error"] = json!({"message": e});
            } else {
                t["status"] = json!("completed");
            }
            if usage_known {
                t["tokensUsed"] = json!(tokens_used);
            }
            if let Some(report) = task_report {
                t["taskReport"] = report;
            }
            if matches!(t["status"].as_str(), Some("failed" | "interrupted")) {
                let thread = srv.threads.lock().unwrap().get(&tid2).cloned();
                if let Some(thread) = thread {
                    let mut snapshot = thread
                        .live
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|live| live.turn.clone())
                        .unwrap_or(json!({"items":[]}));
                    for (key, value) in t.as_object().unwrap() {
                        if key != "items" {
                            snapshot[key] = value.clone();
                        }
                    }
                    let saved = thread
                        .agent
                        .lock()
                        .await
                        .session
                        .append_ui_snapshot(history_len, &snapshot);
                    if let Err(save_error) = saved {
                        t["uiPersistenceError"] = json!(save_error.to_string());
                        srv.notify("error",json!({"error":{"message":"This interrupted response could not be saved. Keep this window open to retain its details."},"threadId":tid2,"turnId":turn_id2,"willRetry":false}));
                    }
                }
            }
            if let Ok(Some(goal)) = goals::finish_turn(
                &tid2,
                &turn_id2,
                t["status"].as_str().unwrap_or("failed"),
                had_work_tools,
                error.as_deref(),
            ) {
                srv.notify("thread/goal/updated", json!({"threadId":tid2,"goal":goal}));
            }
            srv.notify("turn/completed", json!({"threadId": tid2, "turn": t}));
            srv.notify(
                "thread/status/changed",
                json!({"threadId": tid2, "status": {"type": "idle"}}),
            );
        });
        Ok(json!({"turn": turn("inProgress", None)}))
    }

    async fn agent_fix(&self, tid: &str) {
        let t = self.threads.lock().unwrap().get(tid).cloned();
        if let Some(t) = t {
            t.agent.lock().await.fix_dangling();
        }
    }

    fn approvals(&self, thread_id: Option<&str>) -> Vec<Value> {
        let pending = self.pending.lock().unwrap();
        let mut requests: Vec<Value> = pending
            .iter()
            .filter(|(_, request)| thread_id.is_none_or(|id| request.thread_id == id))
            .map(|(id, request)| json!({"id":id,"method":request.method,"params":request.params}))
            .collect();
        requests.sort_by_key(|request| request["id"].as_u64().unwrap_or(0));
        requests
    }

    fn resolve_approval(&self, id: u64, response: Value) {
        let request = self.pending.lock().unwrap().remove(&id);
        if let Some(request) = request {
            let _ = request.reply.send(response);
            self.notify(
                "approval/resolved",
                json!({"requestId":id,"threadId":request.thread_id}),
            );
        }
    }

    fn cancel_approvals(&self, thread_id: Option<&str>) {
        let cancelled: Vec<_> = {
            let mut pending = self.pending.lock().unwrap();
            let ids: Vec<_> = pending
                .iter()
                .filter(|(_, request)| thread_id.is_none_or(|id| request.thread_id == id))
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id).map(|request| (id, request)))
                .collect()
        };
        for (id, request) in cancelled {
            let thread_id = request.thread_id;
            drop(request.reply); // Wake the request task with an empty/declined result.
            self.notify(
                "approval/resolved",
                json!({"requestId":id,"threadId":thread_id}),
            );
        }
    }

    /// Registration and cancellation share the pending lock. An interrupt that
    /// arrives before this task starts cannot leave a late approval waiting forever.
    async fn request(
        &self,
        method: &str,
        params: Value,
        interrupted: Arc<std::sync::atomic::AtomicBool>,
    ) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            if interrupted.load(Ordering::SeqCst) {
                return json!({});
            }
            let thread_id = params["threadId"].as_str().unwrap_or("").to_string();
            let thread = self.threads.lock().unwrap().get(&thread_id).cloned();
            if let Some(thread) = thread {
                let live = thread.live.lock().unwrap();
                if !live
                    .as_ref()
                    .is_some_and(|live| live.is_active() && live.turn["id"] == params["turnId"])
                {
                    return json!({});
                }
            }
            pending.insert(
                id,
                PendingApproval {
                    thread_id,
                    method: method.into(),
                    params: params.clone(),
                    reply: tx,
                },
            );
            if self
                .out
                .send(json!({"id": id, "method": method, "params": params}))
                .is_err()
            {
                pending.remove(&id);
            }
        }
        let r = rx.await.unwrap_or(Value::Null);
        r.get("result").cloned().unwrap_or(json!({}))
    }
}

pub async fn run_channel(
    shared: Shared,
    rx: tokio::sync::mpsc::UnboundedReceiver<Value>,
    out: UnboundedSender<Value>,
) {
    run_channel_until(shared, rx, out, std::future::pending()).await;
}
pub(crate) async fn run_channel_until(
    shared: Shared,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Value>,
    out: UnboundedSender<Value>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) {
    tokio::pin!(shutdown);
    let (account_out, mut account_events) = unbounded_channel::<Value>();
    let account = account::Account::new(account_out);
    let remote = remote::RemoteHub::new(out.clone());
    let images = images::Images::new(out.clone());
    let document_ai = document_ai::DocumentAi::new(out.clone());
    let srv = Arc::new(Server {
        closing: Default::default(),
        remote,
        images,
        document_ai,
        models: models::Catalog::default(),
        out,
        shared,
        threads: Default::default(),
        pending: Default::default(),
        next_id: AtomicU64::new(1),
        scheduled_jobs: Default::default(),
        automation_error: Default::default(),
        workspace_gate: Default::default(),
        account,
        voice: voice::Voice::new(),
        voice_bindings: Default::default(),
    });
    let driver = srv.start_automation_driver();
    let weak = Arc::downgrade(&srv);
    let account_driver = tokio::spawn(async move {
        while let Some(event) = account_events.recv().await {
            let Some(server) = weak.upgrade() else {
                break;
            };
            if event["method"] == "account/updated" {
                if event["params"]["account"]["type"] == "chatgpt" {
                    tokio::join!(
                        server.document_ai.shutdown(),
                        server.stop_all_voice(),
                        server.images.shutdown()
                    );
                }
                server.models.invalidate().await;
            }
            let _ = server.out.send(event);
        }
    });
    loop {
        let msg = tokio::select! {_=&mut shutdown=>break,msg=rx.recv()=>match msg{Some(msg)=>msg,None=>break}};
        match (
            msg.get("method").and_then(|m| m.as_str()),
            msg.get("id").cloned(),
        ) {
            (Some(method), Some(id)) => {
                let (srv, method, params) =
                    (srv.clone(), method.to_string(), msg["params"].clone());
                tokio::spawn(async move {
                    let reply = match srv.handle(&method, params).await {
                        Ok(r) => json!({"id": id, "result": r}),
                        Err((code, message)) => {
                            json!({"id": id, "error": {"code": code, "message": message}})
                        }
                    };
                    let _ = srv.out.send(reply);
                });
            }
            (Some(_), None) => {}
            (None, Some(id)) => {
                if let Some(id) = id.as_u64() {
                    srv.resolve_approval(id, msg);
                }
            }
            _ => {}
        }
    }
    driver.abort();
    account_driver.abort();
    srv.closing.store(true, Ordering::SeqCst);
    tokio::join!(
        srv.document_ai.close(),
        srv.images.close(),
        srv.remote.shutdown(),
        srv.account.shutdown(),
        srv.stop_all_voice()
    );
    srv.stop_scheduled_jobs();
    let threads: Vec<_> = srv.threads.lock().unwrap().values().cloned().collect();
    for thread in threads {
        thread.interrupted.store(true, Ordering::SeqCst);
        if let Some(runner) = thread.runner.lock().unwrap().take() {
            runner.abort();
        }
    }
    srv.cancel_approvals(None);
}

pub async fn run(shared: Shared) -> Result<()> {
    let (out, mut out_rx) = unbounded_channel::<Value>();
    let (in_tx, in_rx) = unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(v) = out_rx.recv().await {
            if stdout.write_all(format!("{v}\n").as_bytes()).await.is_err()
                || stdout.flush().await.is_err()
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    #[test]
    fn an_initial_voice_assistant_message_remains_visible_without_an_invented_user_prompt() {
        let history = vec![
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello from voice"}],"_mcodex_voice":{"id":"voice-first"}}),
        ];
        let turns = history_to_turns(&history, "/fixture");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0]["items"].as_array().unwrap().len(), 1);
        assert_eq!(turns[0]["items"][0]["type"], "agentMessage");
        assert_eq!(turns[0]["items"][0]["text"], "Hello from voice");
        assert_eq!(turns[0]["items"][0]["source"], "voice");
    }

    fn server() -> (Arc<Server>, tokio::sync::mpsc::UnboundedReceiver<Value>) {
        let (out, rx) = unbounded_channel();
        let account = account::Account::new(out.clone());
        let remote = remote::RemoteHub::new(out.clone());
        let images = images::Images::new(out.clone());
        let document_ai = document_ai::DocumentAi::new(out.clone());
        (
            Arc::new(Server {
                closing: Default::default(),
                remote,
                images,
                document_ai,
                models: models::Catalog::default(),
                out,
                shared: Shared {
                    cfg: config::Config::default(),
                    default_model: "local-test".into(),
                    default_sandbox: Sandbox::ReadOnly,
                    default_cwd: std::env::temp_dir(),
                    yolo: false,
                    mcp: Arc::new(mcp::Mcp::default()),
                },
                threads: Default::default(),
                pending: Default::default(),
                next_id: AtomicU64::new(1),
                scheduled_jobs: Default::default(),
                automation_error: Default::default(),
                workspace_gate: Default::default(),
                account,
                voice: voice::Voice::new(),
                voice_bindings: Default::default(),
            }),
            rx,
        )
    }

    #[test]
    fn active_snapshot_restores_partial_items_without_duplicating_persisted_current_turn() {
        let user = |text: &str| json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]});
        let assistant = |text: &str| json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]});
        // A tool round may already persist part of the active turn. Only the
        // history preceding that turn belongs beside the live snapshot.
        let disk = vec![
            user("previous"),
            assistant("answer"),
            user("current"),
            assistant("persisted current partial"),
        ];
        let mut live = LiveTurn::new(2, json!({"id":"live-turn","status":"inProgress"}));
        let item_event =
            |item: Value| json!({"threadId":"thread","turnId":"live-turn","item":item});
        let current_user =
            json!({"type":"userMessage","id":"user","content":[{"type":"text","text":"current"}]});
        live.apply("item/started", &item_event(current_user.clone()));
        live.apply("item/completed", &item_event(current_user));
        live.apply(
            "item/started",
            &item_event(json!({"type":"reasoning","id":"reason","text":""})),
        );
        for delta in ["思考", "中"] {
            live.apply(
                "item/reasoning/summaryTextDelta",
                &json!({"turnId":"live-turn","itemId":"reason","delta":delta}),
            );
        }
        live.apply(
            "item/started",
            &item_event(json!({"type":"agentMessage","id":"message","text":""})),
        );
        live.apply(
            "item/agentMessage/delta",
            &json!({"turnId":"live-turn","itemId":"message","delta":"部分输出"}),
        );
        live.apply(
            "item/started",
            &item_event(json!({"type":"commandExecution","id":"command","aggregatedOutput":null})),
        );
        live.apply(
            "item/commandExecution/outputDelta",
            &json!({"turnId":"live-turn","itemId":"command","delta":"line 1\n"}),
        );
        live.apply(
            "item/commandExecution/outputDelta",
            &json!({"turnId":"live-turn","itemId":"command","delta":"line 2\n"}),
        );
        let restored = turns_with_snapshot(&disk, "/tmp", Some(&live));
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[1]["id"], "live-turn");
        let items = restored[1]["items"].as_array().unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(
            items
                .iter()
                .filter(|item| item["type"] == "userMessage")
                .count(),
            1
        );
        assert_eq!(items[1]["text"], "思考中");
        assert_eq!(items[1]["summary"], json!(["思考中"]));
        assert_eq!(items[2]["text"], "部分输出");
        assert_eq!(items[3]["aggregatedOutput"], "line 1\nline 2\n");
        assert!(!serde_json::to_string(&restored)
            .unwrap()
            .contains("persisted current partial"));
        live.apply(
            "item/completed",
            &item_event(json!({"type":"agentMessage","id":"message","text":"finished"})),
        );
        assert_eq!(live.turn["items"][2]["text"], "finished");
        assert_eq!(live.turn["items"][2]["status"], "completed");
        live.apply(
            "turn/completed",
            &json!({"turn":{"id":"live-turn","status":"completed","items":[]}}),
        );
        assert_eq!(
            turns_with_snapshot(&disk, "/tmp", Some(&live)),
            history_to_turns(&disk, "/tmp")
        );
    }

    #[tokio::test]
    async fn background_approval_can_be_listed_then_resolved_and_removed() {
        let (srv, mut out) = server();
        let requester = srv.clone();
        let task = tokio::spawn(async move {
            requester.request("item/commandExecution/requestApproval", json!({"threadId":"background","turnId":"turn","itemId":"tool","command":"never executed"}), Arc::new(AtomicBool::new(false))).await
        });
        let request = tokio::time::timeout(Duration::from_secs(1), out.recv())
            .await
            .unwrap()
            .unwrap();
        let listed = srv
            .handle("approval/list", json!({"threadId":"background"}))
            .await
            .unwrap();
        assert_eq!(listed["data"], json!([request]));
        assert!(srv
            .handle("approval/list", json!({"threadId":"other"}))
            .await
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty());
        let id = request["id"].as_u64().unwrap();
        srv.resolve_approval(id, json!({"id":id,"result":{"decision":"accept"}}));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap(),
            json!({"decision":"accept"})
        );
        assert!(srv.approvals(None).is_empty());
        let resolved = out.recv().await.unwrap();
        assert_eq!(resolved["method"], "approval/resolved");
        assert_eq!(resolved["params"]["requestId"], id);
    }

    #[tokio::test]
    async fn interrupt_cancels_only_its_thread_and_rejects_late_approval_registration() {
        let (srv, mut out) = server();
        let interrupted = Arc::new(AtomicBool::new(false));
        let requester = srv.clone();
        let flag = interrupted.clone();
        let cancelled = tokio::spawn(async move {
            requester
                .request(
                    "item/fileChange/requestApproval",
                    json!({"threadId":"cancelled","turnId":"turn"}),
                    flag,
                )
                .await
        });
        let first = tokio::time::timeout(Duration::from_secs(1), out.recv())
            .await
            .unwrap()
            .unwrap();
        let requester = srv.clone();
        let other = tokio::spawn(async move {
            requester
                .request(
                    "item/commandExecution/requestApproval",
                    json!({"threadId":"other","turnId":"other-turn"}),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
        });
        let second = tokio::time::timeout(Duration::from_secs(1), out.recv())
            .await
            .unwrap()
            .unwrap();
        interrupted.store(true, Ordering::SeqCst);
        srv.cancel_approvals(Some("cancelled"));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), cancelled)
                .await
                .unwrap()
                .unwrap(),
            json!({})
        );
        assert!(srv.approvals(Some("cancelled")).is_empty());
        assert_eq!(srv.approvals(Some("other")).len(), 1);
        assert_eq!(
            srv.request(
                "item/fileChange/requestApproval",
                json!({"threadId":"cancelled"}),
                interrupted
            )
            .await,
            json!({})
        );
        let resolved = out.recv().await.unwrap();
        assert_eq!(resolved["params"]["requestId"], first["id"]);
        srv.resolve_approval(
            second["id"].as_u64().unwrap(),
            json!({"result":{"decision":"decline"}}),
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), other)
                .await
                .unwrap()
                .unwrap(),
            json!({"decision":"decline"})
        );
        assert!(srv.approvals(None).is_empty());
    }

    #[test]
    fn internal_continuation_timeline_keeps_work_and_fork_uses_visible_user_indices() {
        let user = |text: &str, internal: bool| json!({"type":"message","role":"user","_mcodex_internal":internal,"content":[{"type":"input_text","text":text}]});
        let answer = |text: &str| json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]});
        let items = vec![
            user("Human objective", false),
            answer("Initial result"),
            user("Internal continuation", true),
            json!({"type":"function_call","name":"shell","call_id":"tool","arguments":"{\"command\":[\"true\"]}"}),
            json!({"type":"function_call_output","call_id":"tool","output":"exit code: 0\n"}),
            answer("Continued result"),
            user("Human follow-up", false),
            answer("Final result"),
        ];
        let timeline = history_to_turns(&items, "/tmp");
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0]["items"][0]["internal"], false);
        assert_eq!(timeline[1]["items"][0]["internal"], true);
        assert_eq!(timeline[1]["items"][1]["type"], "commandExecution");
        assert_eq!(timeline[1]["items"][2]["text"], "Continued result");
        assert_eq!(timeline[2]["items"][0]["internal"], false);
        assert_eq!(visible_user_message_index(&items, 0), Some(0));
        assert_eq!(visible_user_message_index(&items, 1), Some(6));
        assert_eq!(visible_user_message_index(&items, 2), None);
        let prefix = &items[..visible_user_message_index(&items, 1).unwrap()];
        assert_eq!(history_to_turns(prefix, "/tmp").len(), 2);
        let mut live = LiveTurn::new(2, json!({"id":"live","status":"inProgress"}));
        live.apply("item/completed",&json!({"turnId":"live","item":{"id":"internal","type":"userMessage","internal":true,"content":[]}}));
        assert_eq!(
            turns_with_snapshot(&items, "/tmp", Some(&live))[1]["items"][0]["internal"],
            true
        );
    }

    #[test]
    fn only_automatic_turns_after_the_first_goal_turn_are_internal() {
        let automatic = json!({"goalContinuation":true});
        assert!(!internal_goal_continuation(
            &automatic,
            Some(&json!({"turnsStarted":0}))
        ));
        assert!(internal_goal_continuation(
            &automatic,
            Some(&json!({"turnsStarted":1}))
        ));
        assert!(!internal_goal_continuation(
            &json!({}),
            Some(&json!({"turnsStarted":8}))
        ));
        assert!(!internal_goal_continuation(&automatic, None));
    }

    #[test]
    fn deletion_selects_only_heartbeats_bound_to_the_deleted_conversation() {
        let tasks = vec![
            json!({"id":"bound","kind":"heartbeat","targetThreadId":"deleted"}),
            json!({"id":"legacy","kind":"heartbeat","threadId":"deleted"}),
            json!({"id":"other","kind":"heartbeat","targetThreadId":"other"}),
            json!({"id":"cron","kind":"cron","targetThreadId":"deleted"}),
        ];
        assert_eq!(
            heartbeat_ids_for_thread(&tasks, "deleted"),
            vec!["bound", "legacy"]
        );
    }
}
