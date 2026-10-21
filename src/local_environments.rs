//! Local environment files and explicit setup/cleanup/action runs.
//! The file shape, platform fallback, revision hashes and shell delta follow
//! the installed Codex 26.1013.40934 local-environment workflow.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc::UnboundedSender, watch},
};

type Result<T> = std::result::Result<T, String>;
const MAX_CONFIG: usize = 1024 * 1024;
const MAX_LOG: usize = 1024 * 1024;
const SHELL_ENV_FILE: &str = "codex-shell-environment.json";
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{field} is required"))
}
fn revision(raw: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(raw.as_bytes()))
}
fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct PlatformScript {
    script: String,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Scripts {
    script: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    darwin: Option<PlatformScript>,
    #[serde(skip_serializing_if = "Option::is_none")]
    linux: Option<PlatformScript>,
    #[serde(skip_serializing_if = "Option::is_none")]
    win32: Option<PlatformScript>,
}
impl Scripts {
    fn normalize(&mut self) {
        self.script = self.script.trim().into();
        for script in [&mut self.darwin, &mut self.linux, &mut self.win32]
            .into_iter()
            .flatten()
        {
            script.script = script.script.trim().into();
        }
    }
    fn selected(&self) -> &str {
        let selected = match platform() {
            "darwin" => &self.darwin,
            "win32" => &self.win32,
            _ => &self.linux,
        };
        selected
            .as_ref()
            .map(|s| s.script.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.script)
    }
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Action {
    name: String,
    command: String,
    #[serde(default)]
    icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
}
#[derive(Clone, Deserialize, Serialize)]
pub struct Environment {
    #[serde(default = "version_one")]
    version: u64,
    name: String,
    setup: Scripts,
    #[serde(skip_serializing_if = "Option::is_none")]
    cleanup: Option<Scripts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<Vec<Action>>,
}
fn version_one() -> u64 {
    1
}
fn parse(raw: &str) -> Result<Environment> {
    let table: toml::Value =
        toml::from_str(raw).map_err(|e| format!("Failed to parse environment config: {e}"))?;
    let mut value = serde_json::to_value(table).map_err(|e| e.to_string())?;
    if let Some(actions) = value["actions"].as_array_mut() {
        actions.retain(|a| {
            a["name"].as_str().is_some_and(|s| !s.trim().is_empty())
                && a["command"].as_str().is_some_and(|s| !s.trim().is_empty())
        });
        for action in actions {
            if !action["icon"]
                .as_str()
                .is_some_and(|s| ["tool", "run", "debug", "test"].contains(&s))
            {
                action["icon"] = Value::Null;
            }
        }
    }
    let mut env: Environment =
        serde_json::from_value(value).map_err(|e| format!("Invalid environment config: {e}"))?;
    if env.version < 1 {
        return Err("Environment version must be at least 1".into());
    }
    env.name = env.name.trim().into();
    env.setup.normalize();
    if let Some(cleanup) = &mut env.cleanup {
        cleanup.normalize();
    }
    for action in env.actions.iter_mut().flatten() {
        action.name = action.name.trim().into();
        action.command = action.command.trim().into();
        if action
            .platform
            .as_ref()
            .is_some_and(|p| !["darwin", "linux", "win32"].contains(&p.as_str()))
        {
            return Err("Invalid environment action platform".into());
        }
    }
    Ok(env)
}

fn context(cwd: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    crate::workspaces::environment_context(cwd, SHELL_ENV_FILE)
}
fn config_path(cwd: &Path, value: &str) -> Result<(PathBuf, PathBuf)> {
    let (root, _) = context(cwd)?;
    let input = Path::new(value);
    let joined = if input.is_absolute() {
        input.to_path_buf()
    } else {
        cwd.join(input)
    };
    let mut path = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                path.pop();
            }
            Component::CurDir => {}
            other => path.push(other.as_os_str()),
        }
    }
    if !path.starts_with(&root) || path.extension().and_then(|e| e.to_str()) != Some("toml") {
        return Err("Environment config must be a TOML file in the selected repository".into());
    }
    let mut current = root.clone();
    for part in path.strip_prefix(&root).unwrap().components() {
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("Environment paths must not pass through symlinks".into())
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    if path.file_name().is_none() {
        return Err("Invalid environment config path".into());
    }
    Ok((root, path))
}
fn raw_file(path: &Path) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_CONFIG as u64
    {
        return Err("Environment config must be a regular file no larger than 1 MiB".into());
    }
    fs::read_to_string(path)
        .map(Some)
        .map_err(|e| e.to_string())
}
fn environment_result(root: &Path, path: &Path, raw: &str) -> Value {
    match parse(raw) {
        Ok(environment) => {
            let directory = path.parent().unwrap();
            let project = if directory.file_name().is_some_and(|n| n == "environments")
                && directory
                    .parent()
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == ".codex"))
            {
                directory.parent().unwrap().parent().unwrap()
            } else {
                directory
            };
            let relative = project.strip_prefix(root).unwrap_or(Path::new("."));
            json!({"type":"success","configPath":path,"cwdRelativeToGitRoot":if relative.as_os_str().is_empty(){Path::new(".")}else{relative},"environment":environment})
        }
        Err(error) => {
            json!({"type":"error","configPath":path,"cwdRelativeToGitRoot":path,"error":{"message":error}})
        }
    }
}
fn read(cwd: &Path, value: &str) -> Result<Value> {
    let (root, path) = config_path(cwd, value)?;
    let raw = raw_file(&path)?;
    Ok(
        json!({"configPath":path,"exists":raw.is_some(),"revision":raw.as_deref().map(revision),"environment":raw.as_deref().map(|raw|environment_result(&root,&path,raw)),"raw":raw}),
    )
}
fn list(cwd: &Path) -> Result<Value> {
    let cwd = cwd.canonicalize().map_err(|e| e.to_string())?;
    let (root, _) = context(&cwd)?;
    let mut cursor = cwd.clone();
    let mut entries = Vec::new();
    for distance in 0..50 {
        let directory = cursor.join(".codex/environments");
        if let Ok(items) = fs::read_dir(&directory) {
            for item in items.flatten() {
                let path = item.path();
                if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                    continue;
                }
                let result = read(&cwd, &path.to_string_lossy());
                let entry = match result {
                    Ok(result) => result["environment"].clone(),
                    Err(error) => {
                        json!({"type":"error","configPath":path,"error":{"message":error}})
                    }
                };
                entries.push((
                    path.file_name()
                        .is_none_or(|name| name != "environment.toml"),
                    distance,
                    path,
                    entry,
                ));
            }
        }
        if cursor == root || !cursor.pop() {
            break;
        }
    }
    entries.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    Ok(
        json!({"environments":entries.into_iter().map(|e|e.3).collect::<Vec<_>>(),"repoRoot":root,"defaultConfigPath":cwd.join(".codex/environments/environment.toml"),"platform":platform()}),
    )
}
fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write(path, bytes, 0o600, None)
}
fn atomic_write(
    path: &Path,
    bytes: &[u8],
    mode: u32,
    permissions: Option<fs::Permissions>,
) -> Result<()> {
    let parent = path.parent().ok_or("Missing parent directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".environment-{:032x}.tmp", rand::random::<u128>()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)
                .map_err(|e| e.to_string())?;
        }
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn save(home: &Path, cwd: &Path, params: &Value) -> Result<Value> {
    let (_, path) = config_path(cwd, text(params, "configPath")?)?;
    let raw = params["raw"].as_str().ok_or("raw is required")?;
    if raw.len() > MAX_CONFIG {
        return Err("Environment config exceeds 1 MiB".into());
    }
    let directory = home.join("local-environment-locks");
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock_path = directory.join(format!(
        "{:x}.lock",
        Sha256::digest(path.to_string_lossy().as_bytes())
    ));
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let before = raw_file(&path)?;
    if before.as_deref() == Some(raw) {
        return Ok(json!({"type":"success","configPath":path,"revision":revision(raw)}));
    }
    if before.as_deref().map(revision).as_deref() != params["expectedRevision"].as_str() {
        return Ok(json!({"type":"conflict","configPath":path}));
    }
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    config_path(cwd, &path.to_string_lossy())?;
    let permissions = fs::metadata(&path).ok().map(|meta| meta.permissions());
    atomic_write(&path, raw.as_bytes(), 0o666, permissions)?;
    Ok(json!({"type":"success","configPath":path,"revision":revision(raw)}))
}

#[derive(Deserialize, Serialize, Default)]
pub struct ShellEnvironment {
    version: u8,
    set: BTreeMap<String, String>,
    exclude: Vec<String>,
}
fn excluded(key: &str) -> bool {
    [
        "CODEX_SOURCE_TREE_PATH",
        "CODEX_WORKTREE_PATH",
        "CODEX_SETUP_EXIT_CODE",
        "OLDPWD",
        "PWD",
        "SHELLOPTS",
        "SHLVL",
        "_",
    ]
    .contains(&key)
        || key.starts_with("BASH_FUNC_")
}
fn parse_env(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            Some((key.into(), value.into()))
        })
        .collect()
}
fn environment_delta(before: &str, after: &str) -> ShellEnvironment {
    let before = parse_env(before);
    let after = parse_env(after);
    let mut delta = ShellEnvironment {
        version: 1,
        ..Default::default()
    };
    for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
        if excluded(key)
            || before.get(key).is_some_and(|v| v.contains(['\r', '\n']))
            || after.get(key).is_some_and(|v| v.contains(['\r', '\n']))
        {
            continue;
        }
        if !after.contains_key(key) {
            delta.exclude.push(key.clone());
        } else if before.get(key) != after.get(key) {
            delta.set.insert(key.clone(), after[key].clone());
        }
    }
    delta
}
pub fn apply_shell_environment(command: &mut Command, cwd: &Path) {
    let Ok((_, Some(path))) = context(cwd) else {
        return;
    };
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    if bytes.len() > MAX_CONFIG {
        return;
    }
    let Ok(env) = serde_json::from_slice::<ShellEnvironment>(&bytes) else {
        return;
    };
    if env.version != 1 {
        return;
    }
    for key in env.exclude {
        if !excluded(&key) {
            command.env_remove(key);
        }
    }
    for (key, value) in env.set {
        if !excluded(&key) {
            command.env(key, value);
        }
    }
}

pub(crate) fn snapshot_shell_environment(cwd: &Path, home: &Path) -> Result<Option<Value>> {
    let (_, Some(path)) = context(cwd)? else {
        return Ok(None);
    };
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if bytes.len() > MAX_CONFIG {
        return Err("Worktree shell environment exceeds 1 MiB".into());
    }
    let value: ShellEnvironment = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if value.version != 1 {
        return Err("Invalid worktree shell environment version".into());
    }
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let name = format!("{hash}.json");
    private_write(
        &home.join("workspace-shell-environments").join(&name),
        &bytes,
    )?;
    Ok(Some(json!({"file":name,"sha256":hash})))
}
pub(crate) fn restore_shell_environment(cwd: &Path, home: &Path, snapshot: &Value) -> Result<()> {
    if snapshot.is_null() {
        return Ok(());
    }
    let hash = text(snapshot, "sha256")?;
    if hash.len() != 64
        || !hash.bytes().all(|b| b.is_ascii_hexdigit())
        || snapshot["file"] != format!("{hash}.json")
    {
        return Err("Invalid shell environment recovery identity".into());
    }
    let source = home
        .join("workspace-shell-environments")
        .join(format!("{hash}.json"));
    if fs::symlink_metadata(&source)
        .map_err(|e| e.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("Shell environment recovery must not be a symlink".into());
    }
    let bytes = fs::read(source).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_CONFIG || format!("{:x}", Sha256::digest(&bytes)) != hash {
        return Err("Shell environment recovery changed".into());
    }
    let value: ShellEnvironment = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if value.version != 1 {
        return Err("Invalid shell environment recovery version".into());
    }
    let (_, Some(path)) = context(cwd)? else {
        return Err("Restored checkout has no Git metadata".into());
    };
    private_write(&path, &bytes)
}

pub(crate) fn prepare_worktree(
    source: &Path,
    target: &Path,
    selected: Option<&str>,
) -> Result<Option<Value>> {
    let Some(selected) = selected else {
        // The reference logs a config-write failure but keeps the checkout.
        let _ = crate::workspaces::environment_selection(target, Some(None));
        return Ok(None);
    };
    let (source_root, original) = config_path(source, selected)?;
    raw_file(&original)?.ok_or("Selected local environment configuration is missing")?;
    let (target_root, _) = context(target)?;
    let relative = original
        .strip_prefix(&source_root)
        .map_err(|e| e.to_string())?;
    let candidate = target_root.join(relative);
    let (_, candidate) = config_path(target, &candidate.to_string_lossy())?;
    let destination = if candidate.exists() {
        Some((candidate, target.to_path_buf()))
    } else {
        let (tracked, ignored) = crate::workspaces::environment_tracking(source, &original)?;
        if tracked {
            None
        } else {
            let (_, target_ignored) = crate::workspaces::environment_tracking(target, &candidate)?;
            if ignored && !target_ignored {
                Some((original, source.to_path_buf()))
            } else {
                fs::create_dir_all(candidate.parent().unwrap()).map_err(|e| e.to_string())?;
                config_path(target, &candidate.to_string_lossy())?;
                let bytes = fs::read(&original).map_err(|e| e.to_string())?;
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&candidate)
                    .map_err(|e| e.to_string())?;
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                file.set_permissions(
                    fs::metadata(&original)
                        .map_err(|e| e.to_string())?
                        .permissions(),
                )
                .map_err(|e| e.to_string())?;
                Some((candidate, target.to_path_buf()))
            }
        }
    };
    let _ = crate::workspaces::environment_selection(
        target,
        Some(destination.as_ref().map(|(path, _)| path.to_str().unwrap())),
    );
    Ok(destination.map(|(path, cwd)| json!({"configPath":path,"configSourceCwd":cwd})))
}

pub(crate) fn cleanup_request(target: &Path, source: &Path) -> Result<Option<Value>> {
    let Some(path) = crate::workspaces::environment_selection(target, None)? else {
        return Ok(None);
    };
    let config_cwd = if config_path(target, &path).is_ok() {
        target
    } else {
        source
    };
    let Ok((_, path)) = config_path(config_cwd, &path) else {
        return Ok(None);
    };
    let Ok(Some(raw)) = raw_file(&path) else {
        return Ok(None);
    };
    let Ok(environment) = parse(&raw) else {
        return Ok(None);
    };
    if environment
        .cleanup
        .as_ref()
        .is_none_or(|s| s.selected().is_empty())
    {
        return Ok(None);
    }
    Ok(Some(
        json!({"configPath":path,"configSourceCwd":config_cwd,"scriptType":"cleanup"}),
    ))
}

struct Job {
    state: Mutex<Value>,
    stop: watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    last_flush: AtomicU64,
}
#[derive(Clone)]
pub struct Environments {
    home: PathBuf,
    out: UnboundedSender<Value>,
    jobs: Arc<Mutex<HashMap<String, Arc<Job>>>>,
}
impl Environments {
    pub fn new(out: UnboundedSender<Value>) -> Self {
        Self {
            home: crate::config::home(),
            out,
            jobs: Default::default(),
        }
    }
    fn publish(&self, job: &Job) {
        let state = job.state.lock().unwrap().clone();
        let _ = private_write(
            &self
                .home
                .join("local-environment-runs")
                .join(format!("{}.json", state["id"].as_str().unwrap())),
            &serde_json::to_vec(&state).unwrap(),
        );
        let _ = self
            .out
            .send(json!({"method":"workspace/environment/updated","params":{"run":state}}));
    }
    fn latest_path(&self, cwd: &Path) -> PathBuf {
        self.home
            .join("local-environment-runs/latest")
            .join(format!(
                "{:x}.json",
                Sha256::digest(cwd.to_string_lossy().as_bytes())
            ))
    }
    fn load_run(&self, id: &str) -> Result<Value> {
        if !id.starts_with("envrun_")
            || id.len() != 39
            || !id[7..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid environment run ID".into());
        }
        if let Some(job) = self.jobs.lock().unwrap().get(id).cloned() {
            return Ok(job.state.lock().unwrap().clone());
        }
        let mut run: Value = serde_json::from_slice(
            &fs::read(
                self.home
                    .join("local-environment-runs")
                    .join(format!("{id}.json")),
            )
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if run["status"] == "running" {
            run["status"] = json!("failed");
            run["error"] = json!(
                "Environment run was interrupted by an app shutdown. Retry setup to continue."
            );
        }
        Ok(run)
    }
    fn latest(&self, cwd: &Path) -> Option<Value> {
        let value: Value = serde_json::from_slice(&fs::read(self.latest_path(cwd)).ok()?).ok()?;
        self.load_run(value["id"].as_str()?).ok()
    }
    pub(crate) fn failed_setup(&self, cwd: &Path, request: &Value, error: &str) -> Value {
        let id = format!("envrun_{:032x}", rand::random::<u128>());
        let state = json!({"id":id,"cwd":cwd,"workspaceRoot":cwd,"configPath":request["configPath"],"configSourceCwd":request["configSourceCwd"],"sourceTreePath":request["sourceTreePath"],"scriptType":"setup","status":"failed","startedAt":now(),"finishedAt":now(),"error":error,"log":""});
        let _ = private_write(
            &self.latest_path(cwd),
            &serde_json::to_vec(&json!({"id":id})).unwrap(),
        );
        let _ = private_write(
            &self
                .home
                .join("local-environment-runs")
                .join(format!("{id}.json")),
            &serde_json::to_vec(&state).unwrap(),
        );
        let _ = self
            .out
            .send(json!({"method":"workspace/environment/updated","params":{"run":state}}));
        state
    }
    pub async fn handle(&self, method: &str, params: &Value) -> Option<Result<Value>> {
        if !method.starts_with("workspace/environment/") {
            return None;
        }
        let result=async {
            if matches!(method, "workspace/environment/status" | "workspace/environment/cancel") {
                let owned;
                let id=if let Some(id)=params["id"].as_str(){id}else{
                    let cwd=PathBuf::from(text(params,"cwd")?).canonicalize().map_err(|e|e.to_string())?;
                    let Some(run)=self.latest(&cwd) else{return Ok(json!({"run":null}));};
                    owned=run["id"].as_str().ok_or("Invalid saved environment run")?.to_string();&owned
                };
                if !id.starts_with("envrun_")||id.len()!=39||!id[7..].bytes().all(|b|b.is_ascii_hexdigit()) {return Err("Invalid environment run ID".into());}
                let job=self.jobs.lock().unwrap().get(id).cloned();
                if let Some(job)=job {
                    if method == "workspace/environment/cancel"{let _=job.stop.send(true);}
                    return Ok(json!({"run":job.state.lock().unwrap().clone()}));
                }
                let mut run:Value=serde_json::from_slice(&fs::read(self.home.join("local-environment-runs").join(format!("{id}.json"))).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
                if run["status"]=="running" {run["status"]=json!("failed");run["error"]=json!("Environment run was interrupted by an app shutdown. Retry setup to continue.");}
                return Ok(json!({"run":run}));
            }
            let cwd=PathBuf::from(text(params,"cwd")?).canonicalize().map_err(|e|e.to_string())?;
            match method {
                "workspace/environment/list"=>{let mut result=list(&cwd)?;result["latestRun"]=json!(self.latest(&cwd));Ok(result)},
                "workspace/environment/read"=>read(&params["configSourceCwd"].as_str().map(PathBuf::from).unwrap_or(cwd.clone()),text(params,"configPath")?),
                "workspace/environment/save"=>save(&self.home,&params["configSourceCwd"].as_str().map(PathBuf::from).unwrap_or(cwd.clone()),params),
                "workspace/environment/selection/read"=>Ok(json!({"configPath":crate::workspaces::environment_selection(&cwd,None)?})),
                "workspace/environment/selection/set"=>{
                    let value=params["configPath"].as_str();if let Some(value)=value {config_path(&cwd,value)?;}
                    Ok(json!({"configPath":crate::workspaces::environment_selection(&cwd,Some(value))?}))
                }
                "workspace/environment/run"=>self.start(&cwd,params).await,
                _=>Err(format!("Unknown local environment method: {method}")),
            }
        }.await;
        Some(result)
    }
    async fn start(&self, cwd: &Path, params: &Value) -> Result<Value> {
        self.start_inner(cwd, params, None).await
    }
    pub(crate) async fn run_owned(&self, cwd: &Path, params: &Value, lease: File) -> Result<Value> {
        let started = self.start_inner(cwd, params, Some(lease)).await?;
        let id = started["run"]["id"]
            .as_str()
            .ok_or("Missing cleanup run ID")?;
        loop {
            let run = self.load_run(id)?;
            if run["status"] == "succeeded" {
                return Ok(run);
            }
            if run["status"] != "running" {
                return Err(run["error"]
                    .as_str()
                    .unwrap_or("Cleanup script failed")
                    .into());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
    async fn start_inner(&self, cwd: &Path, params: &Value, owned: Option<File>) -> Result<Value> {
        let config_cwd = params["configSourceCwd"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(cwd.to_path_buf());
        let (source_root, path) = config_path(&config_cwd, text(params, "configPath")?)?;
        let (root, _) = context(cwd)?;
        let raw = raw_file(&path)?.ok_or("Environment config no longer exists")?;
        let env = parse(&raw)?;
        let loaded = environment_result(&source_root, &path, &raw);
        let relative = loaded["cwdRelativeToGitRoot"].as_str().unwrap_or(".");
        let directory = root
            .join(relative)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !directory.starts_with(&root) {
            return Err("Environment working directory escaped the repository".into());
        }
        let kind = params["scriptType"].as_str().unwrap_or("setup");
        let command = match kind {
            "setup" => env.setup.selected().to_string(),
            "cleanup" => env
                .cleanup
                .as_ref()
                .map(|s| s.selected())
                .unwrap_or("")
                .to_string(),
            "action" => {
                let index = params["actionIndex"]
                    .as_u64()
                    .ok_or("actionIndex is required")? as usize;
                let action = env
                    .actions
                    .as_ref()
                    .and_then(|items| items.get(index))
                    .ok_or("Unknown environment action")?;
                if action.platform.as_deref().is_some_and(|p| p != platform()) {
                    return Err("This action is not available on this platform".into());
                }
                action.command.clone()
            }
            _ => return Err("scriptType must be setup, cleanup or action".into()),
        };
        let lease = match owned {
            Some(lease) => Some(lease),
            None => {
                crate::workspaces::environment_lease(&directory, &self.home, kind != "action")
                    .await?
            }
        };
        let id = format!("envrun_{:032x}", rand::random::<u128>());
        let (stop, receiver) = watch::channel(false);
        let job = Arc::new(Job {
            state: Mutex::new(
                json!({"id":id,"cwd":directory,"workspaceRoot":cwd,"repoRoot":root,"configPath":path,"configSourceCwd":config_cwd,"sourceTreePath":params["sourceTreePath"].as_str().map(String::from).unwrap_or_else(||cwd.to_string_lossy().into_owned()),"environmentName":env.name,"threadId":params["threadId"],"actionName":if kind=="action"{env.actions.as_ref().and_then(|items|items.get(params["actionIndex"].as_u64().unwrap_or(0) as usize)).map(|action|action.name.clone())}else{None},"scriptType":kind,"actionIndex":params["actionIndex"],"status":"running","startedAt":now(),"finishedAt":null,"error":null,"log":"","outputSequence":0}),
            ),
            stop,
            task: Mutex::new(None),
            last_flush: AtomicU64::new(now()),
        });
        self.jobs.lock().unwrap().insert(id.clone(), job.clone());
        private_write(
            &self.latest_path(cwd),
            &serde_json::to_vec(&json!({"id":id})).unwrap(),
        )?;
        self.publish(&job);
        let service = self.clone();
        let worker = job.clone();
        let source = params["sourceTreePath"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or(cwd.to_path_buf());
        let worktree = cwd.to_path_buf();
        let kind = kind.to_string();
        let task = tokio::spawn(async move {
            let _lease = lease;
            let result = service
                .execute(
                    &worker, &directory, &worktree, &source, &command, &kind, receiver,
                )
                .await;
            {
                let mut state = worker.state.lock().unwrap();
                state["status"] = json!(if result.is_ok() {
                    "succeeded"
                } else if *worker.stop.borrow() {
                    "cancelled"
                } else {
                    "failed"
                });
                state["error"] = json!(result.err());
                state["finishedAt"] = json!(now());
                let snapshot = state.clone();
                let _ = private_write(
                    &service
                        .home
                        .join("local-environment-runs")
                        .join(format!("{}.json", snapshot["id"].as_str().unwrap())),
                    &serde_json::to_vec(&snapshot).unwrap(),
                );
                drop(_lease);
                drop(state);
                let _ = service.out.send(
                    json!({"method":"workspace/environment/updated","params":{"run":snapshot}}),
                );
            }
        });
        *job.task.lock().unwrap() = Some(task);
        let state = job.state.lock().unwrap().clone();
        Ok(json!({"run":state}))
    }
    async fn execute(
        &self,
        job: &Arc<Job>,
        cwd: &Path,
        worktree: &Path,
        source: &Path,
        script: &str,
        kind: &str,
        mut stop: watch::Receiver<bool>,
    ) -> Result<()> {
        if *stop.borrow() {
            return Err("Environment run cancelled".into());
        }
        if script.is_empty() {
            if kind == "setup" {
                if let Ok((_, Some(path))) = context(cwd) {
                    let _ = fs::remove_file(path);
                }
            }
            return Ok(());
        }
        let temporary = self
            .home
            .join("tmp/local-environment")
            .join(format!("{:032x}", rand::random::<u128>()));
        fs::create_dir_all(&temporary).map_err(|e| e.to_string())?;
        let clean = TemporaryDirectory(temporary.clone());
        let input = temporary.join("script.sh");
        let before = temporary.join("before.env");
        let after = temporary.join("after.env");
        private_write(&input, script.replace("\r\n", "\n").as_bytes())?;
        let quote =
            |path: &Path| crate::tools::command_text(&[path.to_string_lossy().into_owned()]);
        let wrapper=format!("set -xeo pipefail\ncapture_path={}\nbefore_capture_path={}\nenv > \"$before_capture_path\"\ntrap 'code=$?; if [ \"$code\" -eq 0 ]; then env > \"$capture_path\"; fi' EXIT\n. {}\n",quote(&after),quote(&before),quote(&input));
        let shell = if kind == "action" {
            std::env::var("SHELL")
                .ok()
                .filter(|path| Path::new(path).is_absolute() && Path::new(path).is_file())
                .unwrap_or_else(|| "/bin/bash".into())
        } else {
            "/bin/bash".into()
        };
        let mut command = Command::new(shell);
        command
            .arg(if kind == "action" { "-lc" } else { "-c" })
            .arg(if kind == "action" {
                script.to_string()
            } else {
                wrapper
            })
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        if kind == "action" {
            apply_shell_environment(&mut command, cwd);
        }
        if kind == "setup" {
            command
                .env("CODEX_SOURCE_TREE_PATH", source)
                .env("CODEX_WORKTREE_PATH", worktree);
        }
        command
            .env("COLORTERM", "truecolor")
            .env("FORCE_COLOR", "1")
            .env("TERM", "xterm-256color");
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let mut group = ProcessGroup {
            pid: child.id(),
            armed: true,
        };
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        if *stop.borrow() {
            return Err("Environment run cancelled".into());
        }
        let collect = async {
            let (status, a, b) = tokio::join!(
                child.wait(),
                self.output(job, stdout),
                self.output(job, stderr)
            );
            a?;
            b?;
            status.map_err(|e| e.to_string())
        };
        let status = tokio::select! {result=collect=>result?,_=stop.changed()=>return Err("Environment run cancelled".into())};
        group.armed = false;
        drop(group);
        if !status.success() {
            return Err(format!(
                "{} script exited with code {}",
                kind,
                status.code().unwrap_or(-1)
            ));
        }
        if kind == "setup" {
            if let (Ok(before), Ok(after)) =
                (fs::read_to_string(&before), fs::read_to_string(&after))
            {
                let (_, path) = context(cwd)?;
                if let Some(path) = path {
                    let delta = environment_delta(&before, &after);
                    private_write(&path, &serde_json::to_vec_pretty(&delta).unwrap())?;
                }
            }
        }
        drop(clean);
        Ok(())
    }
    async fn output(&self, job: &Job, mut stream: impl tokio::io::AsyncRead + Unpin) -> Result<()> {
        let mut buffer = [0; 4096];
        let mut pending = Vec::new();
        loop {
            let size = stream.read(&mut buffer).await.map_err(|e| e.to_string())?;
            let decoded = decode_output(&mut pending, &buffer[..size], size == 0);
            if decoded.is_empty() && size == 0 {
                return Ok(());
            }
            {
                let mut state = job.state.lock().unwrap();
                let mut log = state["log"].as_str().unwrap_or("").to_string();
                log.push_str(&decoded);
                if log.len() > MAX_LOG {
                    let mut start = log.len() - MAX_LOG;
                    while !log.is_char_boundary(start) {
                        start += 1;
                    }
                    log = log[start..].to_string();
                }
                state["log"] = json!(log);
                let sequence = state["outputSequence"].as_u64().unwrap_or(0) + 1;
                state["outputSequence"] = json!(sequence);
                let _=self.out.send(json!({"method":"workspace/environment/outputDelta","params":{"id":state["id"],"workspaceRoot":state["workspaceRoot"],"sequence":sequence,"delta":decoded}}));
            };
            let stamp = now();
            let previous = job.last_flush.load(Ordering::Relaxed);
            if stamp.saturating_sub(previous) >= 250
                && job
                    .last_flush
                    .compare_exchange(previous, stamp, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                let state = job.state.lock().unwrap().clone();
                let _ = private_write(
                    &self
                        .home
                        .join("local-environment-runs")
                        .join(format!("{}.json", state["id"].as_str().unwrap())),
                    &serde_json::to_vec(&state).unwrap(),
                );
            }
            if size == 0 {
                return Ok(());
            }
        }
    }
    pub async fn shutdown(&self) {
        let jobs = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for job in jobs {
            let _ = job.stop.send(true);
            let task = job.task.lock().unwrap().take();
            if let Some(task) = task {
                let _ = task.await;
            }
        }
    }
}
pub(crate) fn decode_output(pending: &mut Vec<u8>, chunk: &[u8], finished: bool) -> String {
    pending.extend_from_slice(chunk);
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.push_str(text);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.push_str(std::str::from_utf8(&pending[..valid]).unwrap());
                pending.drain(..valid);
                if let Some(length) = error.error_len() {
                    output.push('\u{fffd}');
                    pending.drain(..length);
                } else {
                    if finished {
                        output.push_str(&String::from_utf8_lossy(pending));
                        pending.clear();
                    }
                    break;
                }
            }
        }
    }
    output
}
struct TemporaryDirectory(PathBuf);
impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct ProcessGroup {
    pid: Option<u32>,
    armed: bool,
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.armed {
            if let Some(pid) = self.pid {
                unsafe extern "C" {
                    fn kill(pid: i32, signal: i32) -> i32;
                }
                unsafe {
                    kill(-(pid as i32), 9);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        home: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "mcodex-environments-{:032x}",
                rand::random::<u128>()
            ));
            let repo = root.join("repository");
            let home = root.join("profile");
            fs::create_dir_all(&repo).unwrap();
            fs::create_dir_all(&home).unwrap();
            let root = root.canonicalize().unwrap();
            let repo = root.join("repository");
            let home = root.join("profile");
            let fixture = Self { root, repo, home };
            fixture.git(&["init", "--template=", "-b", "main"]);
            fixture.git(&["config", "user.name", "Environment fixture"]);
            fixture.git(&["config", "user.email", "fixture@example.invalid"]);
            fs::write(fixture.repo.join("tracked"), "base").unwrap();
            fixture.git(&["add", "tracked"]);
            fixture.git(&["commit", "-m", "fixture"]);
            fixture
        }
        fn git(&self, args: &[&str]) -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&self.repo)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                ])
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().into()
        }
        fn config(&self, relative: &str, raw: &str) -> PathBuf {
            let path = self.repo.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, raw).unwrap();
            path
        }
        fn service(&self) -> Environments {
            let (out, _) = tokio::sync::mpsc::unbounded_channel();
            Environments {
                home: self.home.clone(),
                out,
                jobs: Default::default(),
            }
        }
        fn target(&self) -> PathBuf {
            let target = self
                .root
                .join(format!("target-{:08x}", rand::random::<u32>()));
            self.git(&[
                "worktree",
                "add",
                "--detach",
                target.to_str().unwrap(),
                "HEAD",
            ]);
            target
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    const BASIC: &str = "version=1\nname=' local '\n[setup]\nscript=''\n";
    async fn finished(service: &Environments, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let run = service.load_run(id).unwrap();
                if run["status"] != "running" {
                    return run;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[test]
    fn config_schema_filters_incomplete_actions_and_uses_nonempty_platform_override() {
        let env=parse("name=' dev '\n[setup]\nscript=' echo fallback '\n[setup.darwin]\nscript=' echo mac '\n[[actions]]\nname=''\ncommand='ignored'\n[[actions]]\nname=' Test '\ncommand=' cargo test '\nicon='invalid'\n").unwrap();
        assert_eq!(env.version, 1);
        assert_eq!(env.name, "dev");
        assert_eq!(env.actions.as_ref().unwrap().len(), 1);
        assert!(env.actions.as_ref().unwrap()[0].icon.is_none());
        assert_eq!(
            env.setup.selected(),
            if cfg!(target_os = "macos") {
                "echo mac"
            } else {
                "echo fallback"
            }
        );
        let mut empty = env.setup.clone();
        empty.darwin = Some(PlatformScript {
            script: String::new(),
        });
        assert_eq!(empty.selected(), "echo fallback");
        for raw in ["name='x'","version=0\nname='x'\n[setup]\nscript=''","name='x'\n[setup]\nscript=2","name='x'\n[setup]\nscript=''\n[[actions]]\nname='x'\ncommand='echo x'\nplatform='android'"] {assert!(parse(raw).is_err(),"{raw}");}
    }
    #[test]
    fn discovery_preserves_invalid_files_and_stops_at_the_git_root() {
        let f = Fixture::new();
        f.config(".codex/environments/environment.toml", BASIC);
        f.config("nested/.codex/environments/extra.toml", BASIC);
        f.config("nested/.codex/environments/environment.toml", "broken = [");
        let result = list(&f.repo.join("nested")).unwrap();
        let entries = result["environments"].as_array().unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["type"], "error");
        assert!(entries[0]["configPath"]
            .as_str()
            .unwrap()
            .ends_with("nested/.codex/environments/environment.toml"));
        assert_eq!(entries[1]["cwdRelativeToGitRoot"], ".");
        assert_eq!(entries[2]["cwdRelativeToGitRoot"], "nested");
        let invalid = read(&f.repo, entries[0]["configPath"].as_str().unwrap()).unwrap();
        assert_eq!(invalid["raw"], "broken = [");
        assert!(invalid["revision"].as_str().unwrap().starts_with("sha256:"));
    }
    #[test]
    fn saves_compare_revisions_preserve_conflicts_and_do_not_follow_symlinks() {
        let f = Fixture::new();
        let path = f.repo.join(".codex/environments/environment.toml");
        let first = save(
            &f.home,
            &f.repo,
            &json!({"configPath":path,"raw":BASIC,"expectedRevision":null}),
        )
        .unwrap();
        assert_eq!(first["type"], "success");
        let conflict = save(
            &f.home,
            &f.repo,
            &json!({"configPath":path,"raw":"keep me","expectedRevision":"sha256:wrong"}),
        )
        .unwrap();
        assert_eq!(conflict["type"], "conflict");
        assert_eq!(fs::read_to_string(&path).unwrap(), BASIC);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let saved=save(&f.home,&f.repo,&json!({"configPath":path,"raw":"invalid but editable","expectedRevision":first["revision"]})).unwrap();
        assert_eq!(saved["type"], "success");
        assert_eq!(
            read(&f.repo, path.to_str().unwrap()).unwrap()["environment"]["type"],
            "error"
        );
        assert!(config_path(&f.repo, "../outside.toml").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o640
            );
            let link = f.repo.join("linked");
            symlink(&f.root, &link).unwrap();
            assert!(save(
                &f.home,
                &f.repo,
                &json!({"configPath":link.join("escape.toml"),"raw":BASIC})
            )
            .is_err());
            assert!(!f.root.join("escape.toml").exists());
        }
    }
    #[test]
    fn selected_untracked_environment_is_copied_and_tracking_in_other_branch_skips_setup() {
        let f = Fixture::new();
        let selected = f.config(".codex/environments/environment.toml", BASIC);
        let target = f.target();
        let mapped = prepare_worktree(&f.repo, &target, Some(selected.to_str().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(
            mapped["configPath"],
            json!(target.join(".codex/environments/environment.toml"))
        );
        assert_eq!(
            fs::read_to_string(target.join(".codex/environments/environment.toml")).unwrap(),
            BASIC
        );
        assert_eq!(
            crate::workspaces::environment_selection(&target, None).unwrap(),
            Some(
                target
                    .join(".codex/environments/environment.toml")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        f.git(&["add", ".codex/environments/environment.toml"]);
        f.git(&["commit", "-m", "environment"]);
        let old = f.root.join("old");
        f.git(&[
            "worktree",
            "add",
            "--detach",
            old.to_str().unwrap(),
            "HEAD^",
        ]);
        assert!(
            prepare_worktree(&f.repo, &old, Some(selected.to_str().unwrap()))
                .unwrap()
                .is_none()
        );
        assert!(!old.join(".codex/environments/environment.toml").exists());
        prepare_worktree(&f.repo, &target, None).unwrap();
        assert!(crate::workspaces::environment_selection(&target, None)
            .unwrap()
            .is_none());
    }
    #[test]
    fn environment_delta_records_exports_and_unsets_without_shell_bookkeeping() {
        let delta = environment_delta(
            "A=before\nREMOVED=old\nPWD=/old\nCODEX_SOURCE_TREE_PATH=/source\n",
            "A=after\nADDED=中文\nPWD=/new\nCODEX_SOURCE_TREE_PATH=/changed\n",
        );
        assert_eq!(
            delta.set,
            BTreeMap::from([
                ("A".into(), "after".into()),
                ("ADDED".into(), "中文".into())
            ])
        );
        assert_eq!(delta.exclude, vec!["REMOVED"]);
    }
    #[test]
    fn output_decoder_retains_unicode_split_across_pipe_reads() {
        let input = "设置中文🙂 done".as_bytes();
        let mut pending = Vec::new();
        let mut text = String::new();
        for chunk in input.chunks(2) {
            text.push_str(&decode_output(&mut pending, chunk, false));
        }
        text.push_str(&decode_output(&mut pending, &[], true));
        assert_eq!(text, "设置中文🙂 done");
        assert!(pending.is_empty());
        assert_eq!(decode_output(&mut pending, &[0xf0, 0x9f], false), "");
        assert_eq!(decode_output(&mut pending, &[], true), "�");
    }
    #[tokio::test]
    async fn setup_uses_project_directory_and_captured_exports_reach_later_commands_and_actions() {
        let f = Fixture::new();
        let path=f.config("nested/.codex/environments/environment.toml","name='fixture'\n[setup]\nscript='export MC_ENV_FIXTURE=ready; printf \"%s|%s\" \"$CODEX_SOURCE_TREE_PATH\" \"$CODEX_WORKTREE_PATH\" > marker'\n[[actions]]\nname='Read export'\ncommand='printf %s \"$MC_ENV_FIXTURE\"'\n");
        let service = f.service();
        let started = service
            .start(
                &f.repo.join("nested"),
                &json!({"configPath":path,"sourceTreePath":f.repo}),
            )
            .await
            .unwrap();
        let run = finished(&service, started["run"]["id"].as_str().unwrap()).await;
        assert_eq!(run["status"], "succeeded", "{run}");
        assert_eq!(
            fs::read_to_string(f.repo.join("nested/marker")).unwrap(),
            format!("{}|{}", f.repo.display(), f.repo.join("nested").display())
        );
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf %s \"$MC_ENV_FIXTURE\""]);
        apply_shell_environment(&mut command, &f.repo);
        assert_eq!(command.output().await.unwrap().stdout, b"ready");
        let action = service
            .start(
                &f.repo.join("nested"),
                &json!({"configPath":path,"scriptType":"action","actionIndex":0}),
            )
            .await
            .unwrap();
        let result = finished(&service, action["run"]["id"].as_str().unwrap()).await;
        assert_eq!(result["status"], "succeeded");
        assert!(result["log"].as_str().unwrap().contains("ready"));
        let restarted = f.service();
        assert_eq!(
            restarted.latest(&f.repo.join("nested")).unwrap()["id"],
            result["id"]
        );
        service.shutdown().await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn stopping_a_run_terminates_descendants_and_the_failed_run_can_be_retried() {
        let f = Fixture::new();
        let path=f.config(".codex/environments/environment.toml","name='fixture'\n[setup]\nscript='(while :; do printf x >> writer; sleep .02; done) & wait'\n");
        let service = f.service();
        let start = service
            .start(&f.repo, &json!({"configPath":path}))
            .await
            .unwrap();
        let id = start["run"]["id"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !f.repo.join("writer").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        service
            .handle("workspace/environment/cancel", &json!({"id":id}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(finished(&service, id).await["status"], "cancelled");
        let before = fs::metadata(f.repo.join("writer")).unwrap().len();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(fs::metadata(f.repo.join("writer")).unwrap().len(), before);
        fs::write(
            &path,
            "name='fixed'\n[setup]\nscript='printf repaired > repaired'\n",
        )
        .unwrap();
        let retry = service
            .start(&f.repo, &json!({"configPath":path}))
            .await
            .unwrap();
        assert_eq!(
            finished(&service, retry["run"]["id"].as_str().unwrap()).await["status"],
            "succeeded"
        );
        assert_eq!(fs::read(f.repo.join("repaired")).unwrap(), b"repaired");
        service.shutdown().await;
    }
}
