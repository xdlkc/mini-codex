//! Local desktop product state. RPCs here never access cloud accounts or model credentials.

use serde_json::{json, Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static STORE_LOCK: Mutex<()> = Mutex::new(());
static ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const MAX_STATE_BYTES: usize = 128 * 1024 * 1024;

const METHODS: &[&str] = &[
    "desktop/state/read",
    "desktop/threadMetadata/read",
    "desktop/threadMetadata/set",
    "desktop/threadMetadata/list",
    "desktop/project/create",
    "desktop/project/read",
    "desktop/project/update",
    "desktop/project/delete",
    "desktop/project/list",
    "desktop/document/create",
    "desktop/document/read",
    "desktop/document/update",
    "desktop/document/delete",
    "desktop/document/list",
    "desktop/document/history/list",
    "desktop/document/history/read",
    "desktop/document/restore",
    "desktop/document/export",
    "desktop/document/comments/list",
    "desktop/document/comments/create",
    "desktop/document/comments/update",
    "desktop/document/comments/delete",
    "desktop/settings/read",
    "desktop/settings/update",
];

/// Return `None` for RPCs owned by another module. Each recognized mutation is persisted
/// before success is returned; a later process reads the same JSON document.
pub fn handle(method: &str, params: &Value) -> Option<Result<Value, String>> {
    if !METHODS.contains(&method) {
        return None;
    }
    Some((|| {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Desktop state lock is unavailable".to_string())?;
        Store::new(crate::config::home().join("desktop.json")).handle(method, params)
    })())
}

/// Roll back metadata only after a newly created rollout has been removed.
/// This operation is deliberately absent from the public RPC method list.
pub(crate) fn discard_failed_thread(id: &str) -> Result<(), String> {
    let _guard = STORE_LOCK
        .lock()
        .map_err(|_| "Desktop state lock is unavailable")?;
    Store::new(crate::config::home().join("desktop.json"))
        .handle("desktop/threadMetadata/discard", &json!({"threadId":id}))?;
    Ok(())
}

struct Store {
    path: PathBuf,
}

struct BoundedWriter<W> {
    inner: W,
    written: usize,
}
impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_STATE_BYTES.saturating_sub(self.written) {
            return Err(std::io::Error::other(
                "Desktop state exceeds 128 MiB; current contents were not changed",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.written += count;
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl Store {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn load(&self) -> Result<Value, String> {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
        {
            return Err("Unsafe desktop state file".into());
        }
        let bytes = match File::open(&self.path) {
            Ok(file) => {
                if file
                    .metadata()
                    .map_err(|e| format!("Could not inspect desktop state: {e}"))?
                    .len()
                    > MAX_STATE_BYTES as u64
                {
                    return Err("Desktop state exceeds 128 MiB".into());
                }
                let mut bytes = Vec::new();
                file.take(MAX_STATE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("Could not read desktop state: {e}"))?;
                if bytes.len() > MAX_STATE_BYTES {
                    return Err("Desktop state exceeds 128 MiB".into());
                }
                bytes
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(default_state()),
            Err(e) => return Err(format!("Could not read desktop state: {e}")),
        };
        let state: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Could not parse desktop state: {e}"))?;
        if state["version"] != 1 {
            return Err("Unsupported desktop state version".into());
        }
        for field in ["threadMetadata", "projects", "documents", "settings"] {
            if !state[field].is_object() {
                return Err(format!("Invalid desktop state: {field} must be an object"));
            }
        }
        check_mode(&state["settings"]["mode"])?;
        if !state["settings"]["preferences"].is_object() {
            return Err("Invalid desktop state: preferences must be an object".into());
        }
        crate::documents::validate_collection(&state["documents"])?;
        Ok(state)
    }

    fn save(&self, state: &Value) -> Result<(), String> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create desktop state directory: {e}"))?;
        let temp = parent.join(format!(".desktop-{}.tmp", new_id("write")));
        let result = (|| -> Result<(), String> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp)
                .map_err(|e| format!("Could not create desktop state file: {e}"))?;
            // Enforce the file budget while encoding. Pretty-printing a deeply
            // nested body can multiply its size before a post-encode check.
            {
                let mut writer = BoundedWriter {
                    inner: std::io::BufWriter::new(&mut file),
                    written: 0,
                };
                serde_json::to_writer(&mut writer, state)
                    .map_err(|e| format!("Could not write desktop state: {e}"))?;
                writer
                    .flush()
                    .map_err(|e| format!("Could not flush desktop state: {e}"))?;
            }
            file.sync_all()
                .map_err(|e| format!("Could not sync desktop state: {e}"))?;
            drop(file);
            fs::rename(&temp, &self.path)
                .map_err(|e| format!("Could not replace desktop state: {e}"))?;
            // The file rename is atomic. Sync its directory on Unix for crash durability.
            #[cfg(unix)]
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("Could not sync desktop state directory: {e}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn handle(&self, method: &str, params: &Value) -> Result<Value, String> {
        if !METHODS.contains(&method) && method != "desktop/threadMetadata/discard" {
            return Err(format!("Unknown desktop method: {method}"));
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|e| format!("Could not create desktop state directory: {e}"))?;
        let lock_path = self.path.with_extension("lock");
        if fs::symlink_metadata(&lock_path)
            .is_ok_and(|m| !m.is_file() || m.file_type().is_symlink())
        {
            return Err("Unsafe desktop state lock".into());
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(lock_path)
            .map_err(|e| format!("Could not open desktop state lock: {e}"))?;
        // Concurrent windows and processes must reach the revision check even
        // when another save is delayed by filesystem or CPU contention.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(e) => return Err(format!("Could not lock desktop state: {e}")),
            }
        }
        let empty = Map::new();
        let params = if params.is_null() {
            &empty
        } else {
            params
                .as_object()
                .ok_or("Desktop RPC parameters must be an object")?
        };
        let mut state = self.load()?;
        if let Some(result) =
            crate::documents::handle(&mut state, method, &Value::Object(params.clone()))
        {
            let (value, changed) = result?;
            if changed {
                self.save(&state)?;
            }
            return Ok(value);
        }
        let response = match method {
            "desktop/state/read" => {
                return Ok(json!({
                    "version": state["version"],
                    "threadMetadata": sorted_items(&state["threadMetadata"]),
                    "projects": sorted_items(&state["projects"]),
                    "documents": crate::documents::summaries(&state["documents"] )?,
                    "settings": state["settings"],
                    "capabilities": capabilities(),
                }))
            }
            "desktop/settings/read" => return Ok(json!({"settings": state["settings"]})),
            "desktop/settings/update" => {
                if let Some(mode) = params.get("mode") {
                    check_mode(mode)?;
                    state["settings"]["mode"] = mode.clone();
                }
                if let Some(preferences) = params.get("preferences") {
                    let preferences = preferences
                        .as_object()
                        .ok_or("preferences must be an object")?;
                    validate_preferences(preferences)?;
                    let current = state["settings"]["preferences"].as_object_mut().unwrap();
                    for (key, value) in preferences {
                        current.insert(key.clone(), value.clone());
                    }
                }
                json!({"settings": state["settings"]})
            }
            "desktop/threadMetadata/read" => {
                let id = required_string(params, "threadId")?;
                return Ok(json!({"metadata": state["threadMetadata"].get(id)}));
            }
            "desktop/threadMetadata/discard" => {
                let id = required_string(params, "threadId")?;
                if crate::session::Session::open(Some(id)).is_ok() {
                    return Err("A saved conversation still owns this metadata".into());
                }
                state["threadMetadata"].as_object_mut().unwrap().remove(id);
                json!({"discarded":true})
            }
            "desktop/threadMetadata/list" => {
                validate_filters(params)?;
                return Ok(json!({"items": filter_items(&state["threadMetadata"], params, false)}));
            }
            "desktop/threadMetadata/set" => {
                let id = required_string(params, "threadId")?;
                let mut item = state["threadMetadata"].get(id).cloned().unwrap_or_else(|| {
                    json!({
                        "threadId": id, "mode": state["settings"]["mode"], "pinned": false,
                        "archived": false, "unread": false, "projectId": null,"webSearch":true,"webSearchMode":"auto","webSearchVersion":0,
                    })
                });
                patch_mode(&mut item, params)?;
                if let Some(value) = params.get("approvalsReviewer") {
                    item["approvalsReviewer"] = json!(crate::guardian::Reviewer::parse(value)
                        .map_err(|_| {
                            "approvalsReviewer must be user, guardian_subagent or auto_review"
                        })?
                        .name());
                }
                if let Some(value) = params.get("approvalPolicy") {
                    if !["on-request", "never"]
                        .iter()
                        .any(|policy| value == *policy)
                    {
                        return Err("Unsupported approval policy".into());
                    }
                    item["approvalPolicy"] = value.clone();
                }
                if let Some(value) = params.get("sandboxMode") {
                    if !["read-only", "workspace-write", "danger-full-access"]
                        .iter()
                        .any(|mode| value == *mode)
                    {
                        return Err("Invalid sandbox mode".into());
                    }
                    item["sandboxMode"] = value.clone();
                }
                if let Some(origin) = params.get("forkedFromId") {
                    if !origin.is_null()
                        && origin
                            .as_str()
                            .is_none_or(|value| value.trim().is_empty() || value.len() > 1024)
                    {
                        return Err("forkedFromId must be a non-empty string or null".into());
                    }
                    item["forkedFromId"] = origin.clone();
                }
                if let Some(effort) = params.get("reasoningEffort") {
                    if effort.as_str().is_none_or(|value| {
                        value != "auto" && !crate::models::EFFORTS.contains(&value)
                    }) {
                        return Err("Invalid reasoning effort".into());
                    }
                    item["reasoningEffort"] = effort.clone();
                }
                let previous_mode = item["webSearchMode"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| {
                        if item["webSearch"] == false {
                            "disabled".into()
                        } else {
                            "auto".into()
                        }
                    });
                if let Some(expected) = params.get("expectedWebSearchVersion") {
                    let expected = expected
                        .as_u64()
                        .ok_or("expectedWebSearchVersion must be an integer")?;
                    let actual = item["webSearchVersion"].as_u64().unwrap_or(0);
                    if expected != actual {
                        return Ok(
                            json!({"metadata":item,"conflict":{"expectedVersion":expected,"actualVersion":actual}}),
                        );
                    }
                }
                for key in ["pinned", "archived", "unread", "webSearch"] {
                    if let Some(value) = params.get(key) {
                        if !value.is_boolean() {
                            return Err(format!("{key} must be a boolean"));
                        }
                        item[key] = value.clone();
                    }
                }
                if let Some(value) = params.get("webSearchMode") {
                    let mode = crate::web_search::SearchMode::from_value(value)?;
                    if params
                        .get("webSearch")
                        .is_some_and(|flag| flag.as_bool() != Some(mode.enabled()))
                    {
                        return Err("webSearch and webSearchMode disagree".into());
                    }
                    item["webSearchMode"] = json!(mode.as_str());
                    item["webSearch"] = json!(mode.enabled());
                } else if let Some(value) = params.get("webSearch") {
                    item["webSearchMode"] = json!(if value == true { "auto" } else { "disabled" });
                }
                if params.contains_key("webSearch") || params.contains_key("webSearchMode") {
                    let mode = item["webSearchMode"].as_str().unwrap_or("auto");
                    if mode != previous_mode {
                        item["webSearchVersion"] = json!(item["webSearchVersion"]
                            .as_u64()
                            .unwrap_or(0)
                            .checked_add(1)
                            .ok_or("Search version overflow")?);
                    }
                }
                patch_project(&mut item, params, &state)?;
                item["updatedAt"] = json!(next_timestamp(&item));
                state["threadMetadata"][id] = item.clone();
                json!({"metadata": item})
            }
            "desktop/project/list" => {
                validate_filters(params)?;
                return Ok(json!({"items": filter_items(&state["projects"], params, false)}));
            }
            "desktop/project/read" => {
                let id = required_string(params, "id")?;
                return Ok(json!({"project": lookup(&state, "projects", id)?}));
            }
            "desktop/project/create" => {
                let name = required_string(params, "name")?;
                let id = new_id("project");
                let now = now_ms();
                let mut item = json!({
                    "id": id, "name": name.trim(), "mode": state["settings"]["mode"],
                    "instructions": "", "createdAt": now, "updatedAt": now,
                });
                patch_mode(&mut item, params)?;
                patch_string(&mut item, params, "instructions", false)?;
                patch_project_directory(&mut item, params, true)?;
                check_project_directory_unique(&state, &item)?;
                state["projects"][&id] = item.clone();
                json!({"project": item})
            }
            "desktop/project/update" => {
                let id = required_string(params, "id")?;
                let mut item = lookup(&state, "projects", id)?.clone();
                patch_mode(&mut item, params)?;
                patch_string(&mut item, params, "name", true)?;
                patch_string(&mut item, params, "instructions", false)?;
                patch_project_directory(&mut item, params, false)?;
                check_project_directory_unique(&state, &item)?;
                item["updatedAt"] = json!(next_timestamp(&item));
                state["projects"][id] = item.clone();
                json!({"project": item})
            }
            "desktop/project/delete" => {
                let id = required_string(params, "id")?;
                lookup(&state, "projects", id)?;
                state["projects"].as_object_mut().unwrap().remove(id);
                crate::documents::detach_project(&mut state, id)?;
                for collection in ["threadMetadata"] {
                    for item in state[collection].as_object_mut().unwrap().values_mut() {
                        if item["projectId"].as_str() == Some(id) {
                            item["projectId"] = Value::Null;
                            item["updatedAt"] = json!(next_timestamp(item));
                        }
                    }
                }
                json!({"id": id, "deleted": true})
            }
            _ => unreachable!(),
        };
        self.save(&state)?;
        Ok(response)
    }
}

fn default_state() -> Value {
    json!({
        "version": 1,
        "threadMetadata": {}, "projects": {}, "documents": {},
        "settings": {"mode": "codex", "preferences": {
            "theme": "system", "sidebarCollapsed": false, "sidebarWidth": 260,"webSearchAllowed":true,
        }},
    })
}

fn capabilities() -> Value {
    json!({
        "localPersistence": {"available": true},
        "threadMetadata": {"available": true},
        "desktopProjects": {"available": true},
        "workDocuments": {"available": true},
        "preferences": {"available": true},
        "localScheduledTasks": {"available": true,"reason":"Runs locally while this computer and the app are running"},
        "goals": {"available": true},
        "localGitWorktrees": {"available": true},
        "markdownAndMath": {"available": true},
        "cloudSync": {"available": false, "reason": "No cloud storage or synchronization provider is configured"},
        "sharing": {"available": false, "reason": "No document or conversation sharing service is configured"},
        "voice": {"available": true,"requiresApiKey":true,"permissionsVerified":false,"reason":"Realtime WebRTC and dictation require an API key, microphone permission and provider access"},
        "fileAttachments": {"available":true,"reason":"Local text, PDF, Office and image import; scanned content requires a separate OCR service"},
        "imageGeneration": {"available":true,"requiresApiKey":true,"reason":"Create and edit images after connecting an API key with Images access"},
        "modelCatalog": {"available":true,"reason":"Refresh models from your provider; capability metadata depends on the connection"},
        "connectors": {"available": false, "reason": "Connected account authorization is not implemented by the desktop state service"},
        "cloudExecution": {"available": false, "reason": "No cloud execution provider is configured"},
        "remoteExecution": {"available": true, "requiresConnection":true,"reason": "Connect an SSH host with a Rust agent and its own model connection"},
        "collaboration": {"available": false, "reason": "Documents are stored locally and do not have a collaborative editing service"},
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn next_timestamp(item: &Value) -> u64 {
    now_ms().max(item["updatedAt"].as_u64().unwrap_or(0).saturating_add(1))
}

fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}_{:x}_{:x}_{:x}",
        now_ms(),
        std::process::id(),
        ID_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn required_string<'a>(params: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}

fn check_mode(value: &Value) -> Result<(), String> {
    match value.as_str() {
        Some("chat" | "work" | "codex") => Ok(()),
        _ => Err("mode must be chat, work, or codex".into()),
    }
}

fn patch_mode(item: &mut Value, params: &Map<String, Value>) -> Result<(), String> {
    if let Some(mode) = params.get("mode") {
        check_mode(mode)?;
        item["mode"] = mode.clone();
    }
    Ok(())
}

fn patch_string(
    item: &mut Value,
    params: &Map<String, Value>,
    key: &str,
    nonempty: bool,
) -> Result<(), String> {
    if let Some(value) = params.get(key) {
        let string = value
            .as_str()
            .ok_or_else(|| format!("{key} must be a string"))?;
        if nonempty && string.trim().is_empty() {
            return Err(format!("{key} must not be empty"));
        }
        item[key] = if nonempty {
            json!(string.trim())
        } else {
            value.clone()
        };
    }
    Ok(())
}

/// Project folders belong to the machine handling this RPC. Never create a missing
/// source folder or substitute the project's display name for an execution path.
fn patch_project_directory(
    item: &mut Value,
    params: &Map<String, Value>,
    creating: bool,
) -> Result<(), String> {
    if item["mode"] != "codex" {
        if params.contains_key("path") || params.contains_key("hostId") {
            return Err("Directory bindings are only supported for Codex projects".into());
        }
        return Ok(());
    }
    if let Some(value) = params.get("path") {
        let text = value
            .as_str()
            .filter(|path| !path.trim().is_empty())
            .ok_or("path must be a non-empty absolute directory")?;
        let source = Path::new(text);
        if !source.is_absolute() {
            return Err("path must be an absolute directory".into());
        }
        let directory = source
            .canonicalize()
            .map_err(|error| format!("Cannot open project folder {}: {error}", source.display()))?;
        if !directory.is_dir() {
            return Err("Project path is not a directory".into());
        }
        item["path"] = json!(directory
            .to_str()
            .ok_or("Project path cannot be represented as UTF-8")?);
    } else if creating {
        return Err("Select a source folder for the Codex project".into());
    }
    if let Some(value) = params.get("hostId") {
        let host = value
            .as_str()
            .filter(|host| !host.trim().is_empty())
            .ok_or("hostId must be a non-empty string")?;
        item["hostId"] = json!(host);
    } else if creating {
        item["hostId"] = json!("local");
    }
    Ok(())
}

fn check_project_directory_unique(state: &Value, item: &Value) -> Result<(), String> {
    let Some(path) = item["path"].as_str().filter(|_| item["mode"] == "codex") else {
        return Ok(());
    };
    if let Some(project) = state["projects"]
        .as_object()
        .unwrap()
        .values()
        .find(|other| {
            other["id"] != item["id"] && other["mode"] == "codex" && other["path"] == path
        })
    {
        return Err(format!(
            "This folder is already set up for {}",
            project["name"].as_str().unwrap_or("another project")
        ));
    }
    Ok(())
}

fn lookup<'a>(state: &'a Value, collection: &str, id: &str) -> Result<&'a Value, String> {
    state[collection]
        .get(id)
        .ok_or_else(|| format!("{collection} item not found: {id}"))
}

fn patch_project(
    item: &mut Value,
    params: &Map<String, Value>,
    state: &Value,
) -> Result<(), String> {
    if let Some(project) = params.get("projectId") {
        if !project.is_null() {
            let id = project
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or("projectId must be a non-empty string or null")?;
            lookup(state, "projects", id)?;
        }
        item["projectId"] = project.clone();
    }
    Ok(())
}

fn validate_preferences(preferences: &Map<String, Value>) -> Result<(), String> {
    for key in [
        "globalMiniShortcut",
        "globalDictationShortcut",
        "globalCaptureShortcut",
    ] {
        if preferences.get(key).is_some_and(|value| {
            !matches!(
                value.as_str(),
                Some(
                    "option-space"
                        | "control-option-space"
                        | "option-shift-space"
                        | "command-shift-space"
                        | "disabled"
                )
            )
        }) {
            return Err("Unsupported global shortcut".into());
        }
    }
    if let Some(theme) = preferences.get("theme") {
        if !matches!(theme.as_str(), Some("system" | "light" | "dark")) {
            return Err("theme must be system, light, or dark".into());
        }
    }
    if let Some(collapsed) = preferences.get("sidebarCollapsed") {
        if !collapsed.is_boolean() {
            return Err("sidebarCollapsed must be a boolean".into());
        }
    }
    if preferences
        .get("webSearchAllowed")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("webSearchAllowed must be a boolean".into());
    }
    if let Some(width) = preferences.get("sidebarWidth") {
        if !matches!(width.as_u64(), Some(180..=600)) {
            return Err("sidebarWidth must be an integer between 180 and 600".into());
        }
    }
    Ok(())
}

fn validate_filters(params: &Map<String, Value>) -> Result<(), String> {
    if let Some(mode) = params.get("mode") {
        check_mode(mode)?;
    }
    if let Some(kind) = params.get("kind") {
        if !matches!(kind.as_str(), Some("document" | "code")) {
            return Err("kind must be document or code".into());
        }
    }
    for key in ["pinned", "archived", "unread"] {
        if let Some(value) = params.get(key) {
            if !value.is_boolean() {
                return Err(format!("{key} must be a boolean"));
            }
        }
    }
    if params.get("search").is_some_and(|value| !value.is_string()) {
        return Err("search must be a string".into());
    }
    if let Some(value) = params.get("projectId") {
        if !value.is_null() && !value.is_string() {
            return Err("projectId must be a string or null".into());
        }
    }
    Ok(())
}

fn sorted_items(collection: &Value) -> Vec<Value> {
    let mut items: Vec<Value> = collection.as_object().unwrap().values().cloned().collect();
    items.sort_by(|a, b| {
        b["updatedAt"]
            .as_u64()
            .cmp(&a["updatedAt"].as_u64())
            .then_with(|| {
                a["id"]
                    .as_str()
                    .or(a["threadId"].as_str())
                    .cmp(&b["id"].as_str().or(b["threadId"].as_str()))
            })
    });
    items
}

fn filter_items(
    collection: &Value,
    params: &Map<String, Value>,
    search_content: bool,
) -> Vec<Value> {
    let search = params
        .get("search")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    sorted_items(collection)
        .into_iter()
        .filter(|item| {
            let fields_match = ["mode", "kind", "projectId", "pinned", "archived", "unread"]
                .iter()
                .all(|key| params.get(*key).is_none_or(|value| item[*key] == *value));
            let text = if search_content {
                format!(
                    "{}\n{}",
                    item["title"].as_str().unwrap_or(""),
                    item["content"].as_str().unwrap_or("")
                )
            } else {
                format!(
                    "{}\n{}\n{}",
                    item["name"].as_str().unwrap_or(""),
                    item["instructions"].as_str().unwrap_or(""),
                    item["threadId"].as_str().unwrap_or("")
                )
            };
            fields_match && (search.is_empty() || text.to_lowercase().contains(&search))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestStore {
        directory: PathBuf,
    }

    impl TestStore {
        fn new() -> Self {
            Self {
                directory: std::env::temp_dir().join(new_id("mcodex-desktop-test")),
            }
        }

        fn open(&self) -> Store {
            Store::new(self.directory.join("desktop.json"))
        }

        fn call(&self, method: &str, params: Value) -> Value {
            self.open().handle(method, &params).unwrap()
        }
    }

    impl Drop for TestStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn codex_projects_persist_real_folders_and_host_bindings() {
        let store = TestStore::new();
        let directory = store.directory.join("source");
        fs::create_dir_all(&directory).unwrap();
        let project = store.call(
            "desktop/project/create",
            json!({"name":"Display name", "mode":"codex", "path":directory.join("."), "hostId":"ssh-fixture"}),
        )["project"].clone();
        let canonical = directory.canonicalize().unwrap();
        assert_eq!(project["path"], canonical.to_str().unwrap());
        assert_eq!(project["hostId"], "ssh-fixture");
        store.call(
            "desktop/project/update",
            json!({"id":project["id"], "name":"Renamed"}),
        );
        let reloaded = store
            .open()
            .handle("desktop/project/list", &json!({"mode":"codex"}))
            .unwrap();
        assert_eq!(reloaded["items"][0]["path"], project["path"]);
        assert_eq!(reloaded["items"][0]["hostId"], "ssh-fixture");
        assert_eq!(reloaded["items"][0]["name"], "Renamed");
        assert!(!directory.join(".git").exists());
    }

    #[test]
    fn codex_project_folder_validation_never_creates_missing_sources() {
        let store = TestStore::new();
        store.call("desktop/settings/update", json!({"mode":"codex"}));
        let missing = store.directory.join("missing");
        let file = store.directory.join("file.txt");
        fs::write(&file, "fixture").unwrap();
        let before = fs::read(store.open().path).unwrap();
        for params in [
            json!({"name":"Name only"}),
            json!({"name":"Missing", "path":missing}),
            json!({"name":"Relative", "path":"relative/path"}),
            json!({"name":"File", "path":file}),
            json!({"name":"Invalid host", "path":store.directory, "hostId":null}),
        ] {
            assert!(store
                .open()
                .handle("desktop/project/create", &params)
                .is_err());
            assert_eq!(fs::read(store.open().path).unwrap(), before);
        }
        assert!(!missing.exists());
        assert!(store.call("desktop/project/list", json!({}))["items"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn codex_duplicate_folders_and_failed_rebindings_preserve_saved_project() {
        let store = TestStore::new();
        let directory = store.directory.join("source");
        fs::create_dir_all(&directory).unwrap();
        let project = store.call(
            "desktop/project/create",
            json!({"name":"Original", "path":directory}),
        )["project"]
            .clone();
        let before = fs::read(store.open().path).unwrap();
        assert!(store
            .open()
            .handle(
                "desktop/project/create",
                &json!({"name":"Duplicate", "path":directory.join(".")})
            )
            .is_err());
        assert!(store
            .open()
            .handle(
                "desktop/project/update",
                &json!({"id":project["id"], "name":"Not saved", "path":directory.join("missing")})
            )
            .is_err());
        assert_eq!(fs::read(store.open().path).unwrap(), before);
        assert!(!directory.join("missing").exists());
    }

    #[test]
    fn state_survives_store_reopen_and_preserves_partial_patches() {
        let store = TestStore::new();
        let project = store.call(
            "desktop/project/create",
            json!({"name": "Research", "mode": "work", "instructions": "Cite sources"}),
        )["project"]
            .clone();
        let id = project["id"].as_str().unwrap();
        store.call(
            "desktop/threadMetadata/set",
            json!({"threadId": "thread-1", "mode": "work", "pinned": true, "projectId": id}),
        );
        store.call(
            "desktop/threadMetadata/set",
            json!({"threadId": "thread-1", "unread": true}),
        );
        store.call(
            "desktop/settings/update",
            json!({"mode": "chat", "preferences": {"theme": "dark", "language": "zh-CN"}}),
        );
        store.call(
            "desktop/settings/update",
            json!({"preferences": {"sidebarWidth": 288}}),
        );

        let reloaded = store.call("desktop/state/read", Value::Null);
        assert_eq!(reloaded["threadMetadata"][0]["pinned"], true);
        assert_eq!(reloaded["threadMetadata"][0]["unread"], true);
        assert_eq!(reloaded["threadMetadata"][0]["projectId"], id);
        assert_eq!(reloaded["threadMetadata"][0]["mode"], "work");
        assert_eq!(reloaded["projects"][0]["instructions"], "Cite sources");
        assert_eq!(reloaded["settings"]["mode"], "chat");
        assert_eq!(reloaded["settings"]["preferences"]["theme"], "dark");
        assert_eq!(reloaded["settings"]["preferences"]["language"], "zh-CN");
        assert_eq!(reloaded["settings"]["preferences"]["sidebarWidth"], 288);
        assert_eq!(reloaded["capabilities"]["cloudSync"]["available"], false);
        assert_eq!(fs::read_dir(&store.directory).unwrap().count(), 2);
        assert!(store.directory.join("desktop.lock").is_file());
        assert!(store.directory.join("desktop.json").is_file());
    }

    #[test]
    fn documents_search_update_and_delete_are_persistent() {
        let store = TestStore::new();
        let created = store.call(
            "desktop/document/create",
            json!({"title": "设计笔记", "content": "Rust implementation", "kind": "code"}),
        )["document"]
            .clone();
        let id = created["id"].as_str().unwrap();
        let updated = store.call(
            "desktop/document/update",
            json!({"id": id, "content": "Rust implementation with persistence"}),
        )["document"]
            .clone();
        assert_eq!(updated["createdAt"], created["createdAt"]);
        assert!(updated["updatedAt"].as_u64().unwrap() > created["updatedAt"].as_u64().unwrap());
        let found = store.call(
            "desktop/document/list",
            json!({"search": "PERSISTENCE", "kind": "code"}),
        );
        assert_eq!(found["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            store.call("desktop/document/list", json!({"search": "设计"}))["items"][0]["id"],
            id
        );
        assert_eq!(
            store.call("desktop/document/read", json!({"id": id}))["document"]["content"],
            updated["content"]
        );
        store.call("desktop/document/delete", json!({"id": id}));
        assert!(store
            .open()
            .handle("desktop/document/read", &json!({"id": id}))
            .is_err());
        assert!(store.call("desktop/document/list", json!({}))["items"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rich_document_conflict_keeps_disk_bytes_and_summaries_hide_payloads() {
        let store = TestStore::new();
        let doc = store.call("desktop/document/create",json!({"format":"richtext","body":{"type":"doc","content":[{"type":"paragraph","content":[{"type":"text","text":"longer content"}]}]}}))["document"].clone();
        let id = doc["id"].as_str().unwrap();
        store.call(
            "desktop/document/update",
            json!({"id":id,"expectedRevision":1,"title":"Version two"}),
        );
        let original = fs::read(store.open().path).unwrap();
        let conflict = store.call(
            "desktop/document/update",
            json!({"id":id,"expectedRevision":1,"title":"Stale"}),
        );
        assert_eq!(conflict["conflict"]["actualRevision"], 2);
        assert_eq!(original, fs::read(store.open().path).unwrap());
        let state = store.call("desktop/state/read", json!({}));
        assert_eq!(state["documents"][0]["content"], "longer content\n\n");
        for key in ["body", "sourceMarkdown", "_history", "_comments"] {
            assert!(state["documents"][0].get(key).is_none());
        }
        let reopened = store
            .open()
            .handle("desktop/document/read", &json!({"id":id}))
            .unwrap();
        assert_eq!(reopened["document"]["body"], doc["body"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for name in ["desktop.json", "desktop.lock"] {
                assert_eq!(
                    fs::metadata(store.directory.join(name))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
    }

    #[test]
    fn oversized_or_unsafe_desktop_state_is_never_overwritten() {
        let store = TestStore::new();
        fs::create_dir_all(&store.directory).unwrap();
        let file = File::create(store.open().path).unwrap();
        file.set_len(MAX_STATE_BYTES as u64 + 1).unwrap();
        let error = store
            .open()
            .handle(
                "desktop/document/create",
                &json!({"title":"must not replace"}),
            )
            .unwrap_err();
        assert!(error.contains("128 MiB"));
        assert_eq!(
            fs::metadata(store.open().path).unwrap().len(),
            MAX_STATE_BYTES as u64 + 1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(store.open().path).unwrap();
            let target = store.directory.join("original");
            fs::write(&target, b"protected").unwrap();
            symlink(&target, store.open().path).unwrap();
            assert!(store
                .open()
                .handle("desktop/document/create", &json!({}))
                .unwrap_err()
                .contains("Unsafe"));
            assert_eq!(fs::read(&target).unwrap(), b"protected");
        }
    }

    #[test]
    fn oversized_encoded_state_preserves_saved_bytes_and_cleans_temporary_file() {
        let store = TestStore::new();
        store.call("desktop/document/create", json!({"title":"Keep"}));
        let before = fs::read(store.open().path).unwrap();
        let mut state = default_state();
        state["oversizedFutureField"] = Value::String("a".repeat(MAX_STATE_BYTES + 1));
        let error = store.open().save(&state).unwrap_err();
        assert!(error.contains("128 MiB"), "{error}");
        assert_eq!(fs::read(store.open().path).unwrap(), before);
        assert_eq!(
            fs::read_dir(&store.directory).unwrap().count(),
            2,
            "Failed transaction left a temp file"
        );
    }

    // An actual second process is required: a thread-only test cannot detect a
    // process-local mutex accidentally replacing the stable OS file lock.
    #[test]
    #[ignore = "worker for the isolated cross-process CAS test"]
    fn document_process_worker() {
        let Some(path) = std::env::var_os("MCODEX_DOC_TEST_PATH") else {
            return;
        };
        let id = std::env::var("MCODEX_DOC_TEST_ID").unwrap();
        let result_path = PathBuf::from(std::env::var_os("MCODEX_DOC_TEST_RESULT").unwrap());
        let response = Store::new(PathBuf::from(path)).handle(
            "desktop/document/update",
            &json!({"id":id,"expectedRevision":1,"title":"A concurrent save"}),
        );
        let value = match response {
            Ok(v) => v,
            Err(e) => json!({"error":e}),
        };
        fs::write(result_path, serde_json::to_vec(&value).unwrap()).unwrap();
    }

    #[test]
    fn cross_process_lock_serializes_rich_document_compare_and_swap() {
        use std::process::{Command, Stdio};
        let store = TestStore::new();
        let doc = store.call(
            "desktop/document/create",
            json!({"format":"richtext","body":{"type":"doc","content":[{"type":"paragraph"}]}}),
        )["document"]
            .clone();
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.directory.join("desktop.lock"))
            .unwrap();
        lock.lock().unwrap();
        let results = (0..6)
            .map(|index| store.directory.join(format!("result-{index}.json")))
            .collect::<Vec<_>>();
        let workers = results
            .iter()
            .map(|result| {
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "desktop::tests::document_process_worker",
                        "--ignored",
                    ])
                    .env("MCODEX_DOC_TEST_PATH", store.open().path)
                    .env("MCODEX_DOC_TEST_ID", doc["id"].as_str().unwrap())
                    .env("MCODEX_DOC_TEST_RESULT", result)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            results.iter().all(|path| !path.exists()),
            "Another process bypassed the desktop file lock"
        );
        drop(lock);
        for child in workers {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let values = results
            .iter()
            .map(|path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            values
                .iter()
                .filter(|v| v.get("conflict").is_none())
                .count(),
            1,
            "{values:?}"
        );
        assert!(values.iter().all(|v| v.get("error").is_none()));
        assert_eq!(
            values
                .iter()
                .filter(|v| v["conflict"]["actualRevision"] == 2)
                .count(),
            5
        );
        let read = store.call("desktop/document/read", json!({"id":doc["id"]}));
        assert_eq!(read["document"]["revision"], 2);
        assert_eq!(read["document"]["history"]["count"], 2);
    }

    #[test]
    fn project_deletion_detaches_documents_and_threads_without_deleting_them() {
        let store = TestStore::new();
        let project = store.call(
            "desktop/project/create",
            json!({"name": "Local", "mode": "work"}),
        )["project"]
            .clone();
        let id = project["id"].as_str().unwrap();
        store.call(
            "desktop/threadMetadata/set",
            json!({"threadId": "t", "projectId": id, "archived": true}),
        );
        let document = store.call(
            "desktop/document/create",
            json!({"title": "Keep me", "projectId": id}),
        )["document"]
            .clone();
        assert_eq!(
            store.call(
                "desktop/threadMetadata/list",
                json!({"archived": true, "projectId": id})
            )["items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        store.call(
            "desktop/project/update",
            json!({"id": id, "name": "Renamed"}),
        );
        assert_eq!(
            store.call(
                "desktop/project/list",
                json!({"mode": "work", "search": "renamed"})
            )["items"][0]["id"],
            id
        );
        store.call("desktop/project/delete", json!({"id": id}));
        let state = store.call("desktop/state/read", json!({}));
        assert!(state["projects"].as_array().unwrap().is_empty());
        assert_eq!(state["documents"][0]["id"], document["id"]);
        assert_eq!(state["documents"][0]["projectId"], Value::Null);
        assert_eq!(state["threadMetadata"][0]["projectId"], Value::Null);
        assert_eq!(state["threadMetadata"][0]["archived"], true);
    }

    #[test]
    fn rejected_mutations_and_corrupt_files_do_not_overwrite_saved_state() {
        let store = TestStore::new();
        store.call("desktop/settings/update", json!({"mode": "work"}));
        let before = fs::read(store.open().path).unwrap();
        for (method, params) in [
            ("desktop/settings/update", json!({"mode": "unknown"})),
            (
                "desktop/settings/update",
                json!({"mode": "chat", "preferences": {"theme": "pink"}}),
            ),
            (
                "desktop/threadMetadata/set",
                json!({"threadId": "t", "pinned": "yes"}),
            ),
            (
                "desktop/threadMetadata/set",
                json!({"threadId": "t", "projectId": "missing"}),
            ),
            ("desktop/document/create", json!({"kind": "cloud-sheet"})),
            ("desktop/project/create", json!({"name": "   "})),
        ] {
            assert!(store.open().handle(method, &params).is_err());
            assert_eq!(fs::read(store.open().path).unwrap(), before);
        }
        fs::write(store.open().path, b"{ broken").unwrap();
        assert!(store
            .open()
            .handle("desktop/settings/update", &json!({"mode": "chat"}))
            .is_err());
        assert_eq!(fs::read(store.open().path).unwrap(), b"{ broken");
    }

    #[test]
    fn unsupported_rpc_is_not_claimed() {
        assert!(handle("thread/start", &json!({})).is_none());
    }
}
