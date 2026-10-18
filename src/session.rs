//! Session persistence: JSONL rollouts under `~/.mcodex/sessions/<id>.jsonl`,
//! plus seamless reading of official Codex desktop/CLI rollouts under `~/.codex/sessions/**/*.jsonl`.

use crate::config::home;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Session {
    pub id: String,
    path: PathBuf,
}

pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,
    pub model: String,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub cwd: String,
    pub preview: String,
    pub items: usize,
}

fn dir() -> PathBuf {
    home().join("sessions")
}

fn codex_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

fn collect_jsonl_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_jsonl_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "jsonl") {
            out.push(p);
        }
    }
}

fn all_session_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_jsonl_files(&dir(), &mut files);
    if let Some(cd) = codex_sessions_dir() {
        collect_jsonl_files(&cd, &mut files);
    }
    files.sort_by_key(|p| {
        p.metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    });
    files
}

fn clean_cwd(s: &str) -> String {
    s.strip_prefix("file://").unwrap_or(s).to_string()
}

fn parse_session_file(path: &Path) -> Option<(String, Value, Vec<Value>, SessionInfo)> {
    let text = std::fs::read_to_string(path).ok()?;
    let updated_ms = path
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let first_line = text.lines().next()?;
    let first_val: Value = serde_json::from_str(first_line).ok()?;

    // Format 1: native mcodex session (`{"meta": ...}`)
    if first_val.get("meta").is_some() {
        let mut meta = first_val["meta"].clone();
        let mut items = Vec::new();
        let mut preview = String::new();
        let mut name = None;
        for l in text.lines().skip(1) {
            let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
            if let Some(n) = v["name"].as_str() {
                name = Some(n.to_string());
                meta["name"] = json!(n);
            } else if let Some(it) = v.get("item") {
                if preview.is_empty() && it["role"] == "user" {
                    let t = it["content"][0]["text"].as_str().unwrap_or("").trim();
                    if !t.is_empty() && !t.starts_with('<') {
                        preview = t.chars().take(72).collect();
                    }
                }
                items.push(it.clone());
            }
        }
        let id = meta["id"].as_str().unwrap_or(&stem).to_string();
        let cwd = clean_cwd(meta["cwd"].as_str().unwrap_or(""));
        let model = meta["model"].as_str().unwrap_or("").to_string();
        let created_ms = meta["created_ms"].as_u64().unwrap_or(updated_ms);
        let info = SessionInfo {
            id: id.clone(),
            name,
            model,
            created_ms,
            updated_ms,
            cwd,
            preview,
            items: items.len(),
        };
        return Some((id, meta, items, info));
    }

    // Format 2: official `~/.codex/sessions/**/rollout-*.jsonl`
    let mut id = stem.clone();
    let mut cwd = String::new();
    let mut model = String::new();
    let mut preview = String::new();
    let mut items: Vec<Value> = Vec::new();
    let mut seen_exec_ids = std::collections::HashSet::new();

    for l in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else { continue };
        match v["type"].as_str().unwrap_or("") {
            "session_meta" => {
                if let Some(sid) = v["payload"]["id"].as_str() {
                    id = sid.to_string();
                }
                if let Some(c) = v["payload"]["cwd"].as_str() {
                    cwd = clean_cwd(c);
                }
            }
            "turn_context" => {
                if let Some(c) = v["payload"]["cwd"].as_str() {
                    cwd = clean_cwd(c);
                }
                if let Some(m) = v["payload"]["model"].as_str() {
                    model = m.to_string();
                }
            }
            "response_item" => {
                let p = &v["payload"];
                match p["type"].as_str().unwrap_or("") {
                    "message" => {
                        let role = p["role"].as_str().unwrap_or("");
                        if role == "user" || role == "assistant" {
                            let txt: String = p["content"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|c| {
                                            let s = c["text"].as_str()?.trim();
                                            if s.is_empty() {
                                                return None;
                                            }
                                            if role == "user"
                                                && (s.starts_with('<')
                                                    || s.starts_with("# AGENTS.md instructions")
                                                    || s.starts_with("## Referenced chats with Codex:")
                                                    || s.starts_with("# Files mentioned by the user:"))
                                            {
                                                return None;
                                            }
                                            Some(s)
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            let trimmed = txt.trim();
                            if trimmed.is_empty() {
                                continue;
                            }
                            if role == "user" && preview.is_empty() {
                                preview = trimmed.lines().next().unwrap_or(trimmed).chars().take(72).collect();
                            }
                            items.push(json!({
                                "type": "message",
                                "role": role,
                                "phase": p.get("phase").cloned().unwrap_or(Value::Null),
                                "content": [{"type": if role == "user" { "input_text" } else { "output_text" }, "text": trimmed}]
                            }));
                        }
                    }
                    "reasoning" => {
                        if let Some(arr) = p["summary"].as_array() {
                            if !arr.is_empty() {
                                items.push(p.clone());
                            }
                        }
                    }
                    "function_call" => {
                        let name = p["name"].as_str().unwrap_or("");
                        if name == "shell" || name == "apply_patch" || name.starts_with("mcp__") {
                            items.push(p.clone());
                        }
                    }
                    "function_call_output" => {
                        items.push(p.clone());
                    }
                    _ => {}
                }
            }
            "event_msg" => {
                let p = &v["payload"];
                if p["type"] == "item_completed" {
                    let it = &p["item"];
                    if it["type"] == "CommandExecution" {
                        let cid = it["id"].as_str().unwrap_or("").to_string();
                        if !cid.is_empty() && seen_exec_ids.insert(cid.clone()) {
                            let cmd_str = it["parsed_cmd"]
                                .as_array()
                                .and_then(|a| a.first())
                                .and_then(|x| x["cmd"].as_str())
                                .map(String::from)
                                .or_else(|| {
                                    it["command"].as_array().map(|a| {
                                        if a.len() >= 3 {
                                            a[2].as_str().unwrap_or("").to_string()
                                        } else {
                                            a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" ")
                                        }
                                    })
                                })
                                .unwrap_or_default();
                            let code = it["exit_code"].as_i64().unwrap_or(0);
                            let out = it["aggregated_output"].as_str().unwrap_or("");
                            let dur_ms = it["duration"]["secs"]
                                .as_u64()
                                .map(|s| s * 1000 + it["duration"]["nanos"].as_u64().unwrap_or(0) / 1_000_000);
                            items.push(json!({
                                "type": "function_call",
                                "call_id": cid,
                                "name": "shell",
                                "arguments": json!({"command": ["/bin/zsh", "-lc", cmd_str], "workdir": clean_cwd(it["cwd"].as_str().unwrap_or(&cwd))}).to_string(),
                                "parsed_cmd": it["parsed_cmd"],
                                "duration_ms": dur_ms,
                            }));
                            items.push(json!({
                                "type": "function_call_output",
                                "call_id": cid,
                                "output": format!("exit code: {code}\n{out}"),
                            }));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if items.is_empty() {
        return None;
    }
    let meta = json!({
        "id": id,
        "cwd": cwd,
        "model": model,
        "created_ms": updated_ms,
    });
    let info = SessionInfo {
        id: id.clone(),
        name: None,
        model,
        created_ms: updated_ms,
        updated_ms,
        cwd,
        preview,
        items: items.len(),
    };
    Some((id, meta, items, info))
}

impl Session {
    pub fn set_name(&self, name: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&self.path) {
            let _ = writeln!(f, "{}", json!({"name": name}));
        }
    }

    pub fn delete(&self) -> bool {
        std::fs::remove_file(&self.path).is_ok()
    }

    pub fn create(cwd: &str, model: &str) -> Result<Self> {
        std::fs::create_dir_all(dir())?;
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
        let id = ms.to_string();
        let path = dir().join(format!("{id}.jsonl"));
        let mut f = std::fs::File::create(&path)?;
        writeln!(f, "{}", json!({"meta": {"id": id, "cwd": cwd, "model": model, "created_ms": ms as u64}}))?;
        Ok(Self { id, path })
    }

    pub fn append(&self, item: &Value) {
        if self.path.starts_with(dir()) {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&self.path) {
                let _ = writeln!(f, "{}", json!({"item": item}));
            }
        }
    }

    /// Resume by id (prefix or substring allowed) or the most recent when `id` is None.
    pub fn open(id: Option<&str>) -> Result<(Self, Vec<Value>, Value)> {
        let files = all_session_files();
        if files.is_empty() {
            return Err(anyhow!("no sessions yet"));
        }
        for path in files.into_iter().rev() {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            if let Some(target) = id {
                if !stem.contains(target) {
                    // Check if internal session_meta id matches target
                    let Some((sid, meta, items, _)) = parse_session_file(&path) else { continue };
                    if sid == target || sid.starts_with(target) {
                        return Ok((Self { id: sid, path }, items, meta));
                    }
                    continue;
                }
            }
            if let Some((sid, meta, items, _)) = parse_session_file(&path) {
                if id.is_none() || sid == id.unwrap() || stem.contains(id.unwrap()) {
                    return Ok((Self { id: sid, path }, items, meta));
                }
            }
        }
        Err(anyhow!("session not found"))
    }
}

pub fn list() -> Vec<SessionInfo> {
    let mut v: Vec<SessionInfo> = all_session_files()
        .into_iter()
        .filter_map(|p| parse_session_file(&p).map(|(_, _, _, info)| info))
        .collect();
    v.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    v
}
