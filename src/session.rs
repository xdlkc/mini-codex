//! Session persistence: JSONL rollouts under `~/.mcodex/sessions/<id>.jsonl`,
//! plus seamless reading of official Codex desktop/CLI rollouts under `~/.codex/sessions/**/*.jsonl`.

use crate::config::home;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone)]
pub struct Session {
    pub id: String,
    path: PathBuf,
    store: PathBuf,
    read_only: bool,
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
    pub archived: bool,
    pub read_only: bool,
}

fn dir() -> PathBuf {
    home().join("sessions")
}

fn codex_sessions_dir() -> Option<PathBuf> {
    if std::env::var("MCODEX_NO_CODEX_IMPORT").as_deref() == Ok("1") {
        return None;
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex").join("sessions"))
}

fn collect_jsonl_files(root: &Path, out: &mut Vec<PathBuf>) {
    if std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_jsonl_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "jsonl") {
            out.push(p);
        }
    }
}

struct SessionFile {
    path: PathBuf,
    read_only: bool,
}

fn all_session_files() -> Vec<SessionFile> {
    let mut native = Vec::new();
    collect_jsonl_files(&dir(), &mut native);
    let mut files: Vec<SessionFile> = native
        .into_iter()
        .map(|path| SessionFile {
            path,
            read_only: false,
        })
        .collect();
    if let Some(cd) = codex_sessions_dir() {
        let mut imported = Vec::new();
        collect_jsonl_files(&cd, &mut imported);
        files.extend(imported.into_iter().map(|path| SessionFile {
            path,
            read_only: true,
        }));
    }
    files.sort_by_key(|file| modified_ms(&file.path));
    files
}

fn modified_ms(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn metadata_path(store: &Path, id: &str) -> PathBuf {
    // Official IDs are untrusted input. Encoding prevents path traversal.
    let key: String = id.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
    store.join("session-metadata").join(format!("{key}.json"))
}

fn read_metadata(store: &Path, id: &str) -> Value {
    std::fs::read_to_string(metadata_path(store, id))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({}))
}

pub(crate) fn is_archived_at(store: &Path, id: &str) -> std::result::Result<bool, String> {
    let bytes = match std::fs::read(metadata_path(store, id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("Read session metadata: {error}")),
    };
    let metadata: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Read session metadata: {error}"))?;
    Ok(metadata["archived"] == true)
}

static METADATA_LOCK: Mutex<()> = Mutex::new(());

fn update_metadata(store: &Path, id: &str, change: impl FnOnce(&mut Value)) -> Result<()> {
    let _lock = METADATA_LOCK
        .lock()
        .map_err(|_| anyhow!("session metadata lock poisoned"))?;
    let path = metadata_path(store, id);
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let mut metadata = read_metadata(store, id);
    change(&mut metadata);
    metadata["updated_ms"] = json!(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64);
    let temp = parent.join(format!(".{:032x}.tmp", rand::random::<u128>()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(&metadata)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

fn load_session_file(
    file: &SessionFile,
    store: &Path,
) -> Option<(String, Value, Vec<Value>, SessionInfo)> {
    let (id, mut meta, items, mut info) = parse_session_file(&file.path)?;
    let local = read_metadata(store, &id);
    if let Some(name) = local["name"].as_str() {
        info.name = Some(name.into());
        meta["name"] = json!(name);
    }
    info.archived = local["archived"].as_bool().unwrap_or(false);
    info.read_only |= file.read_only;
    info.updated_ms = info
        .updated_ms
        .max(local["updated_ms"].as_u64().unwrap_or(0));
    meta["archived"] = json!(info.archived);
    meta["read_only"] = json!(info.read_only);
    Some((id, meta, items, info))
}

fn clean_cwd(s: &str) -> String {
    s.strip_prefix("file://").unwrap_or(s).to_string()
}

const MAX_UI_TURN_BYTES: usize = 8 * 1024 * 1024;
struct JsonSize {
    bytes: usize,
    limit: usize,
}
impl std::io::Write for JsonSize {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes) {
            return Err(std::io::Error::other("UI turn snapshot exceeds 8 MiB"));
        }
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn ui_json_size(value: &Value, limit: usize) -> Result<usize> {
    let mut counter = JsonSize { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.bytes)
}
fn ui_links(value: &Value) -> Result<()> {
    if let Some(url) = value.get("url") {
        anyhow::ensure!(
            url.as_str().and_then(crate::web_search::safe_url).is_some(),
            "UI source URL is unsafe"
        );
    }
    for key in ["sources", "citations", "parts"] {
        if let Some(values) = value.get(key) {
            let values = values
                .as_array()
                .context("UI source metadata must be an array")?;
            for value in values {
                ui_links(value)?;
            }
        }
    }
    Ok(())
}
/// UI snapshots contain observed display data only. They never become a Responses
/// item. Filter the existing user bubble before measuring/cloning its image data.
fn prepare_ui_turn(turn: &Value) -> Result<Value> {
    let fields = turn
        .as_object()
        .context("UI turn snapshot must be an object")?;
    anyhow::ensure!(
        matches!(turn["status"].as_str(), Some("failed" | "interrupted"))
            || turn["status"] == "completed"
                && turn["items"].as_array().is_some_and(|items| items
                    .iter()
                    .any(|item| item["approvalReview"].is_object())),
        "UI snapshots require an interrupted/failed turn or a completed approval review"
    );
    anyhow::ensure!(
        turn["id"].as_str().is_some_and(|s| !s.trim().is_empty()
            && s.len() <= 1024
            && !s.chars().any(char::is_control)),
        "UI snapshot requires a valid turn id"
    );
    let items = turn["items"]
        .as_array()
        .context("UI snapshot items must be an array")?;
    anyhow::ensure!(items.len() <= 4096, "UI snapshot exceeds its item limit");
    let mut used = 128usize;
    let mut filtered = serde_json::Map::new();
    for (key, value) in fields {
        if key == "items" {
            continue;
        }
        let key_size = ui_json_size(&json!(key), MAX_UI_TURN_BYTES.saturating_sub(used))?;
        let value_size =
            ui_json_size(value, MAX_UI_TURN_BYTES.saturating_sub(used + key_size + 2))?;
        used = used
            .checked_add(key_size + value_size + 2)
            .context("UI snapshot byte count overflow")?;
        anyhow::ensure!(used <= MAX_UI_TURN_BYTES, "UI turn snapshot exceeds 8 MiB");
        filtered.insert(key.clone(), value.clone());
    }
    let mut visible = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for item in items {
        if item["type"] == "userMessage" {
            continue;
        }
        anyhow::ensure!(
            item.is_object()
                && matches!(
                    item["type"].as_str(),
                    Some(
                        "agentMessage"
                            | "reasoning"
                            | "webSearch"
                            | "commandExecution"
                            | "fileChange"
                            | "mcpToolCall"
                            | "dynamicToolCall"
                    )
                ),
            "UI snapshot contains an unsupported display item"
        );
        let id = item["id"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
            .context("UI snapshot item requires a valid id")?;
        anyhow::ensure!(seen.insert(id), "UI snapshot contains duplicate item ids");
        if let Some(status) = item.get("status") {
            anyhow::ensure!(
                matches!(
                    status.as_str(),
                    Some(
                        "completed"
                            | "failed"
                            | "interrupted"
                            | "incomplete"
                            | "cancelled"
                            | "canceled"
                            | "declined"
                            | "rejected"
                            | "denied"
                            | "aborted"
                    )
                ),
                "UI snapshot items must have terminal states"
            );
        }
        if matches!(item["type"].as_str(), Some("agentMessage" | "webSearch")) {
            ui_links(item)?;
        }
        let size = ui_json_size(item, MAX_UI_TURN_BYTES.saturating_sub(used + 1))?;
        used += size + 1;
        visible.push(item.clone());
    }
    filtered.insert("items".into(), json!(visible));
    Ok(Value::Object(filtered))
}

fn parse_session_file(path: &Path) -> Option<(String, Value, Vec<Value>, SessionInfo)> {
    let mut file = std::fs::File::open(path).ok()?;
    file.lock_shared().ok()?;
    let mut text = String::new();
    file.read_to_string(&mut text).ok()?;
    let updated_ms = file
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
        if !meta.is_object() {
            return None;
        }
        meta.as_object_mut().unwrap().remove("_mcodex_ui_turns");
        let mut ui_turns = serde_json::Map::new();
        let mut invalid_ui = 0u64;
        let mut items = Vec::new();
        let mut preview = String::new();
        let mut name = None;
        for l in text.lines().skip(1) {
            let Ok(v) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            if let Some(snapshot) = v.get("uiTurn") {
                let parsed = (|| -> Result<(usize, Value)> {
                    anyhow::ensure!(
                        v.as_object().is_some_and(|fields| fields.len() == 1)
                            && l.len() <= MAX_UI_TURN_BYTES,
                        "Invalid UI wrapper"
                    );
                    let index = usize::try_from(
                        snapshot["historyIndex"]
                            .as_u64()
                            .context("Invalid UI history index")?,
                    )?;
                    Ok((index, prepare_ui_turn(&snapshot["turn"])?))
                })();
                match parsed {
                    Ok((index, turn)) => {
                        ui_turns.insert(index.to_string(), turn);
                    }
                    Err(_) => invalid_ui += 1,
                }
                continue;
            }
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
        ui_turns.retain(|index, _| {
            index
                .parse::<usize>()
                .ok()
                .and_then(|n| items.get(n))
                .is_some_and(|item| item["type"] == "message" && item["role"] == "user")
        });
        if !ui_turns.is_empty() {
            meta["_mcodex_ui_turns"] = Value::Object(ui_turns);
        }
        if invalid_ui > 0 {
            meta["_mcodex_ui_turns_ignored"] = json!(invalid_ui);
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
            archived: false,
            read_only: false,
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
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
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
                                                    || s.starts_with(
                                                        "## Referenced chats with Codex:",
                                                    )
                                                    || s.starts_with(
                                                        "# Files mentioned by the user:",
                                                    ))
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
                                preview = trimmed
                                    .lines()
                                    .next()
                                    .unwrap_or(trimmed)
                                    .chars()
                                    .take(72)
                                    .collect();
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
                                            a.iter()
                                                .filter_map(|x| x.as_str())
                                                .collect::<Vec<_>>()
                                                .join(" ")
                                        }
                                    })
                                })
                                .unwrap_or_default();
                            let code = it["exit_code"].as_i64().unwrap_or(0);
                            let out = it["aggregated_output"].as_str().unwrap_or("");
                            let dur_ms = it["duration"]["secs"].as_u64().map(|s| {
                                s * 1000 + it["duration"]["nanos"].as_u64().unwrap_or(0) / 1_000_000
                            });
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
        archived: false,
        read_only: true,
    };
    Some((id, meta, items, info))
}

impl Session {
    /// Imported official rollouts are always read-only. Native paths are checked
    /// again before each write so replacing a file with a symlink cannot redirect it.
    pub fn is_read_only(&self) -> bool {
        if self.read_only {
            return true;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::symlink_metadata(&self.path).is_ok_and(|metadata| metadata.nlink() != 1) {
                return true;
            }
        }
        let root = self.store.join("sessions");
        if std::fs::symlink_metadata(&root).is_ok_and(|m| m.file_type().is_symlink())
            || std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.file_type().is_symlink())
        {
            return true;
        }
        match (root.canonicalize(), self.path.canonicalize()) {
            (Ok(root), Ok(path)) => path.parent() != Some(root.as_path()),
            _ => true,
        }
    }

    pub fn set_name(&self, name: &str) {
        let _ = self.try_set_name(name);
    }

    pub fn try_set_name(&self, name: &str) -> Result<()> {
        update_metadata(&self.store, &self.id, |metadata| {
            metadata["name"] = json!(name)
        })
    }

    /// Archive only changes this app's metadata, including for imported threads.
    pub fn archive(&self) -> Result<()> {
        update_metadata(&self.store, &self.id, |metadata| {
            metadata["archived"] = json!(true)
        })
    }

    pub fn restore(&self) -> Result<()> {
        update_metadata(&self.store, &self.id, |metadata| {
            metadata["archived"] = json!(false)
        })
    }

    /// Official rollout files must never be deleted by this application.
    pub fn delete(&self) -> bool {
        if self.is_read_only() {
            return false;
        }
        if std::fs::remove_file(&self.path).is_err() {
            return false;
        }
        let _ = std::fs::remove_file(metadata_path(&self.store, &self.id));
        true
    }

    pub fn create(cwd: &str, model: &str) -> Result<Self> {
        Self::create_in(&home(), cwd, model)
    }

    pub(crate) fn create_in(store: &Path, cwd: &str, model: &str) -> Result<Self> {
        let sessions = store.join("sessions");
        std::fs::create_dir_all(&sessions)?;
        if std::fs::symlink_metadata(&sessions)?
            .file_type()
            .is_symlink()
        {
            bail!("session storage directory must not be a symlink");
        }
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();
        loop {
            let id = format!("{ms}-{:032x}", rand::random::<u128>());
            let path = sessions.join(format!("{id}.jsonl"));
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error).context("creating session"),
            };
            let written = writeln!(
                file,
                "{}",
                json!({"meta": {"id": id, "cwd": cwd, "model": model, "created_ms": ms as u64}})
            )
            .and_then(|_| file.sync_all());
            if let Err(error) = written {
                drop(file);
                let _ = std::fs::remove_file(&path);
                return Err(error).context("saving session header");
            }
            return Ok(Self {
                id,
                path,
                store: store.to_path_buf(),
                read_only: false,
            });
        }
    }

    pub fn append(&self, item: &Value) {
        let _ = self.append_batch_checked(std::slice::from_ref(item));
    }

    /// Append a transcript batch as complete JSONL records. Report write failures
    /// so the client can retain its unsaved transcript and retry.
    pub fn append_batch_checked(&self, items: &[Value]) -> Result<()> {
        anyhow::ensure!(
            !self.is_read_only(),
            "Imported or replaced sessions cannot be changed"
        );
        let mut bytes = Vec::new();
        for item in items {
            serde_json::to_writer(&mut bytes, &json!({"item":item}))?;
            bytes.push(b'\n');
        }
        self.append_jsonl_checked(&bytes, None)
    }

    pub(crate) fn append_ui_snapshot(&self, history_index: usize, turn: &Value) -> Result<()> {
        anyhow::ensure!(
            !self.is_read_only(),
            "Imported or replaced sessions cannot be changed"
        );
        let turn = prepare_ui_turn(turn)?;
        let wrapper = json!({"uiTurn":{"historyIndex":history_index,"turn":turn}});
        ui_json_size(&wrapper, MAX_UI_TURN_BYTES)?;
        let mut bytes = serde_json::to_vec(&wrapper)?;
        bytes.push(b'\n');
        self.append_jsonl_checked(&bytes, Some(history_index))
    }

    fn append_jsonl_checked(&self, bytes: &[u8], user_index: Option<usize>) -> Result<()> {
        use std::io::{BufRead, Seek, SeekFrom};
        anyhow::ensure!(
            !self.is_read_only(),
            "Imported or replaced sessions cannot be changed"
        );
        if bytes.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;
        file.lock()?;
        anyhow::ensure!(
            !self.is_read_only(),
            "Session changed before the transcript could be saved"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let opened = file.metadata()?;
            let current = std::fs::symlink_metadata(&self.path)?;
            anyhow::ensure!(
                opened.dev() == current.dev() && opened.ino() == current.ino(),
                "Session file was replaced before it could be saved"
            );
        }
        if let Some(index) = user_index {
            file.seek(SeekFrom::Start(0))?;
            let mut records = std::io::BufReader::new(&mut file).lines();
            let header = records.next().context("Session header is missing")??;
            let header = serde_json::from_str::<Value>(&header)?;
            anyhow::ensure!(
                header["meta"].is_object(),
                "UI snapshots require native session metadata"
            );
            let stem = self.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            anyhow::ensure!(
                header["meta"]["id"].as_str().unwrap_or(stem) == self.id,
                "Session identity changed before the UI snapshot could be saved"
            );
            let mut model_index = 0;
            let mut found = false;
            for line in records {
                let line = line?;
                let Ok(record) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if record.get("uiTurn").is_some() || record["name"].is_string() {
                    continue;
                }
                if let Some(item) = record.get("item") {
                    if model_index == index {
                        found = item["type"] == "message" && item["role"] == "user";
                        break;
                    }
                    model_index += 1;
                }
            }
            anyhow::ensure!(
                found,
                "UI snapshot history index must refer to an existing user message"
            );
        }
        let original = file.seek(SeekFrom::End(0))?;
        if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_data()) {
            let rollback = file.set_len(original).and_then(|_| file.sync_data());
            if rollback.is_err() {
                anyhow::bail!(
                    "Could not save the transcript or roll back the partial write: {error}"
                );
            }
            return Err(error.into());
        }
        Ok(())
    }

    /// Exact IDs take priority. A partial ID must identify a single session.
    /// Archived sessions can be opened explicitly so they can be restored.
    pub fn open(id: Option<&str>) -> Result<(Self, Vec<Value>, Value)> {
        let store = home();
        let mut matches = Vec::new();
        for file in all_session_files().into_iter().rev() {
            let Some((sid, meta, items, info)) = load_session_file(&file, &store) else {
                continue;
            };
            if id.is_none() && info.archived {
                continue;
            }
            let stem = file.path.file_stem().unwrap_or_default().to_string_lossy();
            let exact = id.is_some_and(|target| sid == target);
            if id.is_none()
                || exact
                || id.is_some_and(|target| sid.starts_with(target) || stem.contains(target))
            {
                let session = Self {
                    id: sid,
                    path: file.path,
                    store: store.clone(),
                    read_only: info.read_only,
                };
                if id.is_none() || exact {
                    return Ok((session, items, meta));
                }
                matches.push((session, items, meta));
            }
        }
        match matches.len() {
            0 => Err(anyhow!("session not found")),
            1 => Ok(matches.pop().unwrap()),
            _ => Err(anyhow!("ambiguous session ID; use the full ID")),
        }
    }
}

pub fn list() -> Vec<SessionInfo> {
    list_filtered(Some(false))
}

pub fn list_archived() -> Vec<SessionInfo> {
    list_filtered(Some(true))
}

pub(crate) fn list_all() -> Vec<SessionInfo> {
    list_filtered(None)
}

fn list_filtered(archived: Option<bool>) -> Vec<SessionInfo> {
    let store = home();
    let mut sessions: Vec<SessionInfo> = all_session_files()
        .into_iter()
        .filter_map(|file| load_session_file(&file, &store).map(|(_, _, _, info)| info))
        .filter(|info| archived.is_none_or(|archived| info.archived == archived))
        .collect();
    sessions.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mc-session-test-{}-{:032x}",
                std::process::id(),
                rand::random::<u128>()
            ));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn store(&self) -> PathBuf {
            self.0.join("mcodex")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn concurrent_session_creation_preserves_every_file() {
        let fixture = Fixture::new();
        let store = fixture.store();
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let store = store.clone();
                std::thread::spawn(move || {
                    (0..32)
                        .map(|n| {
                            let cwd = format!("worker-{worker}-{n}");
                            let session = Session::create_in(&store, &cwd, "test").unwrap();
                            (session.id, session.path, cwd)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let sessions: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        let ids: std::collections::HashSet<_> = sessions.iter().map(|(id, _, _)| id).collect();
        assert_eq!(ids.len(), 256);
        assert_eq!(
            std::fs::read_dir(store.join("sessions")).unwrap().count(),
            256
        );
        for (_, path, cwd) in sessions {
            let (_, meta, _, _) = parse_session_file(&path).unwrap();
            assert_eq!(meta["cwd"], cwd);
        }
    }

    #[test]
    fn imported_rollout_is_immutable_and_metadata_is_local_and_reversible() {
        let fixture = Fixture::new();
        let path = fixture.0.join("official.jsonl");
        let original = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"official-id\",\"cwd\":\"/project\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"hello\"}]}}\n"
        );
        std::fs::write(&path, original).unwrap();
        let session = Session {
            id: "official-id".into(),
            path: path.clone(),
            store: fixture.store(),
            read_only: true,
        };
        assert!(session.is_read_only());
        session.append(&json!({"type": "message", "role": "user"}));
        session.try_set_name("Renamed locally").unwrap();
        session.archive().unwrap();
        assert!(!session.delete());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let file = SessionFile {
            path: path.clone(),
            read_only: true,
        };
        let (_, meta, _, info) = load_session_file(&file, &fixture.store()).unwrap();
        assert!(info.archived);
        assert!(info.read_only);
        assert_eq!(meta["name"], "Renamed locally");
        // An official rollout copied into native storage is still read-only.
        let disguised = SessionFile {
            path: path.clone(),
            read_only: false,
        };
        assert!(
            load_session_file(&disguised, &fixture.store())
                .unwrap()
                .3
                .read_only
        );
        session.restore().unwrap();
        assert!(
            !load_session_file(&file, &fixture.store())
                .unwrap()
                .3
                .archived
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn native_archive_restore_keeps_history_and_delete_removes_only_native_file() {
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        let item = json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]});
        session.append(&item);
        session.archive().unwrap();
        let file = SessionFile {
            path: session.path.clone(),
            read_only: false,
        };
        assert!(
            load_session_file(&file, &fixture.store())
                .unwrap()
                .3
                .archived
        );
        session.restore().unwrap();
        let (_, _, items, info) = load_session_file(&file, &fixture.store()).unwrap();
        assert!(!info.archived);
        assert_eq!(items, vec![item]);
        assert!(session.delete());
        assert!(!session.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_native_symlink_cannot_write_or_delete_another_rollout() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        let outside = fixture.0.join("official.jsonl");
        std::fs::write(&outside, "untouched\n").unwrap();
        std::fs::remove_file(&session.path).unwrap();
        symlink(&outside, &session.path).unwrap();
        session.append(&json!({"bad": true}));
        assert!(!session.delete());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "untouched\n");
    }

    #[test]
    fn metadata_ids_cannot_escape_the_store() {
        let fixture = Fixture::new();
        update_metadata(&fixture.store(), "../../outside", |value| {
            value["name"] = json!("local")
        })
        .unwrap();
        assert!(metadata_path(&fixture.store(), "../../outside")
            .starts_with(fixture.store().join("session-metadata")));
        assert!(!fixture.0.join("outside").exists());
    }

    fn ui_turn(status: &str) -> Value {
        json!({"id":"ui_turn","status":status,"startedAt":1,"completedAt":2,"items":[{"type":"webSearch","id":"web_ui","providerId":"ws_fixture","status":status,"queries":["fixture"],"sources":[{"url":"https://example.test/source"}]},{"type":"agentMessage","id":"partial_ui","status":status,"text":"Observed partial 中文🙂"}]})
    }
    fn user_item(text: &str) -> Value {
        json!({"type":"message","role":"user","content":[{"type":"input_text","text":text}]})
    }
    #[test]
    fn ui_snapshots_survive_reopen_without_entering_model_history_or_copying_user_images() {
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        let user = user_item("Human prompt");
        session.append_batch_checked(&[user.clone()]).unwrap();
        let mut snapshot = ui_turn("interrupted");
        snapshot["items"].as_array_mut().unwrap().insert(0,json!({"type":"userMessage","id":"user_ui","attachments":[{"image_url":"X".repeat(MAX_UI_TURN_BYTES+1)}]}));
        session.append_ui_snapshot(0, &snapshot).unwrap();
        let file = SessionFile {
            path: session.path.clone(),
            read_only: false,
        };
        let (_, meta, items, info) = load_session_file(&file, &fixture.store()).unwrap();
        assert_eq!(items, vec![user.clone()]);
        assert_eq!(info.items, 1);
        assert_eq!(meta["_mcodex_ui_turns"]["0"]["status"], "interrupted");
        assert_eq!(
            meta["_mcodex_ui_turns"]["0"]["items"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(std::fs::metadata(&session.path).unwrap().len() < 8192);
        let reopened = Session {
            id: session.id.clone(),
            path: session.path.clone(),
            store: fixture.store(),
            read_only: false,
        };
        let changed = ui_turn("failed");
        reopened.append_ui_snapshot(0, &changed).unwrap();
        reopened
            .append_batch_checked(&[user_item("Follow-up")])
            .unwrap();
        let (_, meta, items, _) = load_session_file(&file, &fixture.store()).unwrap();
        assert_eq!(meta["_mcodex_ui_turns"]["0"], changed);
        assert_eq!(items, vec![user, user_item("Follow-up")]);
        assert!(!serde_json::to_string(&items).unwrap().contains("uiTurn"));
        assert!(!serde_json::to_string(&crate::agent::model_history(items))
            .unwrap()
            .contains("_mcodex_ui_turns"));
    }
    #[test]
    fn ui_snapshot_validation_rejects_nonterminal_unsafe_or_oversized_data_without_writes() {
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        session
            .append_batch_checked(&[
                user_item("Prompt"),
                json!({"type":"message","role":"assistant","content":[]}),
            ])
            .unwrap();
        let before = std::fs::read(&session.path).unwrap();
        for bad in ["completed", "inProgress", "searching", ""] {
            assert!(session.append_ui_snapshot(0, &ui_turn(bad)).is_err());
        }
        let mut invalid = ui_turn("failed");
        invalid["items"][0]["status"] = json!("searching");
        assert!(session.append_ui_snapshot(0, &invalid).is_err());
        invalid = ui_turn("failed");
        invalid["items"][0]["sources"][0]["url"] = json!("javascript:alert(1)");
        assert!(session.append_ui_snapshot(0, &invalid).is_err());
        invalid = ui_turn("failed");
        invalid["items"][0]["type"] = json!("function_call");
        assert!(session.append_ui_snapshot(0, &invalid).is_err());
        invalid = ui_turn("failed");
        invalid["items"][1]["id"] = invalid["items"][0]["id"].clone();
        assert!(session.append_ui_snapshot(0, &invalid).is_err());
        invalid = ui_turn("failed");
        invalid["items"][1]["text"] = json!("a".repeat(MAX_UI_TURN_BYTES));
        assert!(session.append_ui_snapshot(0, &invalid).is_err());
        assert!(session.append_ui_snapshot(1, &ui_turn("failed")).is_err());
        assert!(session.append_ui_snapshot(999, &ui_turn("failed")).is_err());
        assert_eq!(std::fs::read(&session.path).unwrap(), before);
    }
    #[test]
    fn malformed_ui_wrappers_are_ignored_and_old_rollouts_still_load_model_items() {
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        let user = user_item("Legacy user");
        session.append_batch_checked(&[user.clone()]).unwrap();
        let rows = [
            json!({"uiTurn":{"historyIndex":0,"turn":ui_turn("completed")}}),
            json!({"uiTurn":{"historyIndex":0,"turn":ui_turn("interrupted")},"item":{"type":"function_call","name":"must_not_enter_history"}}),
            json!({"uiTurn":{"historyIndex":"0","turn":ui_turn("failed")}}),
            json!({"uiTurn":{"historyIndex":99,"turn":ui_turn("interrupted")}}),
        ];
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&session.path)
            .unwrap();
        for row in rows {
            writeln!(file, "{row}").unwrap();
        }
        drop(file);
        let (_, meta, items, _) = parse_session_file(&session.path).unwrap();
        assert_eq!(items, vec![user]);
        assert!(meta["_mcodex_ui_turns"].is_null());
        assert_eq!(meta["_mcodex_ui_turns_ignored"], 3);
        session
            .append_ui_snapshot(0, &ui_turn("interrupted"))
            .unwrap();
        let (_, meta, items, _) = parse_session_file(&session.path).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(meta["_mcodex_ui_turns"]["0"]["status"], "interrupted");
    }
    #[cfg(unix)]
    #[test]
    fn readonly_and_replaced_sessions_cannot_receive_ui_snapshot_writes() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let session = Session::create_in(&fixture.store(), "/project", "test").unwrap();
        session
            .append_batch_checked(&[user_item("Prompt")])
            .unwrap();
        let original = std::fs::read(&session.path).unwrap();
        let imported = Session {
            id: session.id.clone(),
            path: session.path.clone(),
            store: fixture.store(),
            read_only: true,
        };
        assert!(imported
            .append_ui_snapshot(0, &ui_turn("interrupted"))
            .is_err());
        assert_eq!(std::fs::read(&session.path).unwrap(), original);
        let target = fixture.0.join("official-data.jsonl");
        std::fs::write(&target, &original).unwrap();
        std::fs::remove_file(&session.path).unwrap();
        symlink(&target, &session.path).unwrap();
        assert!(session.append_ui_snapshot(0, &ui_turn("failed")).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
        std::fs::remove_file(&session.path).unwrap();
        std::fs::hard_link(&target, &session.path).unwrap();
        assert!(session.append_ui_snapshot(0, &ui_turn("failed")).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
        std::fs::remove_file(&session.path).unwrap();
        std::fs::write(
            &session.path,
            format!(
                "{}\n{}\n",
                json!({"meta":{"id":"different-session"}}),
                json!({"item":user_item("Another user's native session")})
            ),
        )
        .unwrap();
        let replaced = std::fs::read(&session.path).unwrap();
        assert!(session.append_ui_snapshot(0, &ui_turn("failed")).is_err());
        assert_eq!(std::fs::read(&session.path).unwrap(), replaced);
    }
}
