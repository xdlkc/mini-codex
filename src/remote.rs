//! Explicit SSH profiles and a bounded JSONL proxy to a remote Rust app-server.
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD},
    Engine,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{ChildStdin, Command},
    sync::{mpsc::UnboundedSender, oneshot, watch, Mutex as AsyncMutex},
    task::JoinHandle,
};

type Result<T> = std::result::Result<T, String>;
const MAX_FRAME: usize = 32 * 1024 * 1024;
const MAX_STDERR: usize = 16 * 1024;
static PROFILE_LOCK: Mutex<()> = Mutex::new(());
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}
fn text<'a>(p: &'a Value, key: &str) -> Result<&'a str> {
    p[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}
fn safe_text(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Profile {
    id: String,
    name: String,
    host: String,
    user: Option<String>,
    port: u16,
    identity_file: Option<String>,
    remote_cwd: String,
    agent_path: String,
    created_at: u64,
    updated_at: u64,
}
impl Profile {
    fn validate(&self) -> Result<()> {
        if !self.id.starts_with("rp_")
            || self.id.len() != 35
            || !self.id[3..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid remote profile id".into());
        }
        if !safe_text(&self.name, 200) {
            return Err("Remote profile name must contain 1 to 200 bytes".into());
        }
        if !safe_text(&self.host, 253)
            || self.host.starts_with('-')
            || !self
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-:%".contains(&b))
        {
            return Err("SSH host must be a host name, IPv6 address, or SSH config alias".into());
        }
        if self.port == 0 {
            return Err("SSH port must be between 1 and 65535".into());
        }
        if self.user.as_ref().is_some_and(|u| {
            !safe_text(u, 128)
                || u.starts_with('-')
                || !u
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        }) {
            return Err("SSH user contains unsupported characters".into());
        }
        if self
            .identity_file
            .as_ref()
            .is_some_and(|s| !safe_text(s, 4096) || !Path::new(s).is_absolute())
        {
            return Err(
                "identityFile must be an absolute local path; key contents are never read".into(),
            );
        }
        if !safe_text(&self.remote_cwd, 4096) || !self.remote_cwd.starts_with('/') {
            return Err("remoteCwd must be an absolute remote POSIX path".into());
        }
        if !safe_text(&self.agent_path, 4096) || self.agent_path.starts_with('-') {
            return Err("agentPath must name the remote executable".into());
        }
        Ok(())
    }
    fn remote_command(&self) -> String {
        format!(
            "{} app-server --listen stdio:// --cd {}",
            quote(&self.agent_path),
            quote(&self.remote_cwd)
        )
    }
    fn apply(&mut self, p: &Value) -> Result<()> {
        for (key, value) in p
            .as_object()
            .ok_or("Remote profile parameters must be an object")?
        {
            match key.as_str() {
                "id" => {}
                "name" | "host" | "remoteCwd" | "agentPath" => {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("{key} must be a string"))?
                        .to_owned();
                    match key.as_str() {
                        "name" => self.name = value,
                        "host" => self.host = value,
                        "remoteCwd" => self.remote_cwd = value,
                        _ => self.agent_path = value,
                    }
                }
                "user" | "identityFile" => {
                    let value = if value.is_null() {
                        None
                    } else {
                        Some(
                            value
                                .as_str()
                                .ok_or_else(|| format!("{key} must be a string or null"))?
                                .to_owned(),
                        )
                    };
                    if key == "user" {
                        self.user = value
                    } else {
                        self.identity_file = value
                    }
                }
                "port" => {
                    self.port = value
                        .as_u64()
                        .and_then(|n| u16::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or("port must be between 1 and 65535")?
                }
                _ => return Err(format!("Unknown remote profile field: {key}")),
            }
        }
        self.validate()
    }
}

#[derive(Clone)]
struct Profiles {
    root: PathBuf,
}

#[derive(Clone)]
struct HostKey {
    line: String,
    fingerprint: String,
}
#[derive(Clone)]
struct Probe {
    profile: Profile,
    keys: Vec<HostKey>,
    expires_at: u64,
}
fn host_token(profile: &Profile) -> String {
    if profile.port == 22 {
        profile.host.clone()
    } else {
        format!("[{}]:{}", profile.host, profile.port)
    }
}
fn key_string<'a>(bytes: &mut &'a [u8]) -> Result<&'a [u8]> {
    if bytes.len() < 4 {
        return Err("Invalid SSH public key encoding".into());
    }
    let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    *bytes = &bytes[4..];
    if length > bytes.len() {
        return Err("Invalid SSH public key encoding".into());
    }
    let (value, rest) = bytes.split_at(length);
    *bytes = rest;
    Ok(value)
}
fn host_key(profile: &Profile, line: &str) -> Result<HostKey> {
    if line.len() > 32 * 1024 || line.contains('\n') || line.contains('\r') {
        return Err("Invalid SSH host key line".into());
    }
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 || fields[0] != host_token(profile) {
        return Err("Host key is not bound to this profile host and port".into());
    }
    let raw = STANDARD
        .decode(fields[2])
        .map_err(|_| "Invalid SSH public key base64")?;
    let mut remaining = raw.as_slice();
    if key_string(&mut remaining)? != fields[1].as_bytes() {
        return Err("SSH key algorithm does not match its data".into());
    }
    match fields[1] {
        "ssh-ed25519" => {
            if key_string(&mut remaining)?.len() != 32 {
                return Err("Invalid ed25519 host key".into());
            }
        }
        "ssh-rsa" => {
            let exponent = key_string(&mut remaining)?;
            let modulus = key_string(&mut remaining)?;
            if exponent.is_empty() || exponent.len() > 8 || !(128..=2049).contains(&modulus.len()) {
                return Err("Invalid RSA host key".into());
            }
        }
        algorithm if algorithm.starts_with("ecdsa-sha2-") => {
            let curve = key_string(&mut remaining)?;
            let point = key_string(&mut remaining)?;
            let expected = match curve {
                b"nistp256" => 65,
                b"nistp384" => 97,
                b"nistp521" => 133,
                _ => return Err("Unsupported ECDSA host key curve".into()),
            };
            if algorithm.strip_prefix("ecdsa-sha2-").unwrap().as_bytes() != curve
                || point.len() != expected
                || point.first() != Some(&4)
            {
                return Err("Invalid ECDSA host key".into());
            }
        }
        _ => return Err("Unsupported SSH host key algorithm".into()),
    }
    if !remaining.is_empty() {
        return Err("Unexpected trailing SSH public key data".into());
    }
    Ok(HostKey {
        line: format!(
            "{} {} {}",
            host_token(profile),
            fields[1],
            STANDARD.encode(&raw)
        ),
        fingerprint: format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&raw))),
    })
}
fn known_hosts_command(path: &Path) -> Result<String> {
    let path = path.to_str().ok_or("App known_hosts path must use UTF-8")?;
    // OpenSSH expands ${...} before command parsing, even inside quotes.
    if !safe_text(path, 8192) || path.contains("${") {
        return Err(
            "App known_hosts path must not contain control characters or environment expansions"
                .into(),
        );
    }
    Ok(format!("/bin/cat {}", quote(&path.replace('%', "%%"))))
}
async fn limited_read<R: AsyncRead + Unpin>(mut reader: R, maximum: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    (&mut reader)
        .take(maximum as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "Cannot read SSH keyscan output")?;
    if data.len() > maximum {
        return Err("SSH keyscan output exceeds its byte limit".into());
    }
    Ok(data)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    profiles: Vec<Profile>,
}
impl Profiles {
    fn transaction<T>(&self, action: impl FnOnce(&mut Stored) -> Result<(T, bool)>) -> Result<T> {
        let _guard = PROFILE_LOCK
            .lock()
            .map_err(|_| "Remote profile lock is unavailable")?;
        fs::create_dir_all(&self.root)
            .map_err(|e| format!("Cannot create remote profile storage: {e}"))?;
        let path = self.root.join("remote-profiles.json");
        let lock_path = self.root.join("remote-profiles.lock");
        for p in [&path, &lock_path] {
            if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink() || !m.is_file()) {
                return Err("Unsafe remote profile storage path".into());
            }
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(&lock_path)
            .map_err(|e| format!("Cannot open remote profile lock: {e}"))?;
        let until = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(e) => return Err(format!("Cannot acquire remote profile lock: {e}")),
            }
        }
        let mut state = match File::open(&path) {
            Ok(mut f) => {
                let mut data = Vec::new();
                Read::by_ref(&mut f)
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut data)
                    .map_err(|e| e.to_string())?;
                if data.len() > 1024 * 1024 {
                    return Err("Remote profile store exceeds 1 MiB".into());
                }
                serde_json::from_slice::<Stored>(&data)
                    .map_err(|_| "Invalid remote profile store")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stored {
                version: 1,
                profiles: Vec::new(),
            },
            Err(e) => return Err(format!("Cannot read remote profiles: {e}")),
        };
        if state.version != 1 || state.profiles.len() > 100 {
            return Err("Unsupported remote profile store".into());
        }
        let mut seen = std::collections::HashSet::new();
        for p in &state.profiles {
            p.validate()?;
            if !seen.insert(&p.id) {
                return Err("Duplicate remote profile ids".into());
            }
        }
        let (value, changed) = action(&mut state)?;
        if changed {
            let tmp = self.root.join(format!(".remote-{}.tmp", id("write")));
            let result = (|| {
                let mut options = OpenOptions::new();
                options.create_new(true).write(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut f = options.open(&tmp).map_err(|e| e.to_string())?;
                f.write_all(&serde_json::to_vec(&state).unwrap())
                    .and_then(|_| f.sync_all())
                    .map_err(|e| e.to_string())?;
                fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                File::open(&self.root)
                    .and_then(|f| f.sync_all())
                    .map_err(|e| e.to_string())
            })();
            if result.is_err() {
                let _ = fs::remove_file(&tmp);
            }
            result?;
        }
        Ok(value)
    }
    fn get(&self, id: &str) -> Result<Profile> {
        self.transaction(|s| {
            Ok((
                s.profiles
                    .iter()
                    .find(|p| p.id == id)
                    .cloned()
                    .ok_or("Remote profile not found")?,
                false,
            ))
        })
    }
    fn rpc(&self, method: &str, p: &Value) -> Result<Value> {
        self.transaction(|s| match method {
            "remote/profile/list" => Ok((json!({"data":s.profiles}), false)),
            "remote/profile/create" => {
                if s.profiles.len() >= 100 {
                    return Err("At most 100 remote profiles can be stored".into());
                }
                if !p["id"].is_null() {
                    return Err("Profile ids are generated by the server".into());
                }
                let mut profile = Profile {
                    id: id("rp"),
                    name: String::new(),
                    host: String::new(),
                    user: None,
                    port: 22,
                    identity_file: None,
                    remote_cwd: String::new(),
                    agent_path: "mcodex".into(),
                    created_at: now(),
                    updated_at: now(),
                };
                profile.apply(p)?;
                s.profiles.push(profile.clone());
                Ok((json!({"profile":profile}), true))
            }
            "remote/profile/update" => {
                let profile = s
                    .profiles
                    .iter_mut()
                    .find(|profile| Some(profile.id.as_str()) == p["id"].as_str())
                    .ok_or("Remote profile not found")?;
                profile.apply(p)?;
                profile.updated_at = now();
                Ok((json!({"profile":profile}), true))
            }
            "remote/profile/delete" => {
                let id = text(p, "id")?;
                let before = s.profiles.len();
                s.profiles.retain(|profile| profile.id != id);
                if before == s.profiles.len() {
                    return Err("Remote profile not found".into());
                }
                Ok((json!({"id":id,"deleted":true}), true))
            }
            _ => Err("Unknown remote profile RPC".into()),
        })
    }
}

struct Status {
    state: &'static str,
    error: Option<String>,
    initialize: Option<Value>,
    stderr: Vec<u8>,
}
#[derive(Clone)]
struct Approval {
    remote_id: Value,
    method: String,
    params: Value,
}
struct Connection {
    id: String,
    profile_id: String,
    created_at: u64,
    status: Mutex<Status>,
    writer: AsyncMutex<Option<ChildStdin>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<Value>>>>,
    approvals: Mutex<HashMap<String, Approval>>,
    sequence: AtomicU64,
    stop: watch::Sender<bool>,
    job: Mutex<Option<JoinHandle<()>>>,
    out: UnboundedSender<Value>,
    rpc_timeout: Duration,
}
impl Connection {
    fn public(&self) -> Value {
        let s = self.status.lock().unwrap();
        json!({"id":self.id,"profileId":self.profile_id,"state":s.state,"createdAt":self.created_at,"error":s.error,"initialize":s.initialize,"stderr":String::from_utf8_lossy(&s.stderr)})
    }
    fn emit_status(&self) {
        let _ = self
            .out
            .send(json!({"method":"remote/status","params":{"connection":self.public()}}));
    }
    fn active(&self) -> bool {
        matches!(
            self.status.lock().unwrap().state,
            "connecting" | "connected"
        )
    }
    fn approval_id(&self, id: &Value) -> String {
        format!("remote:{}:{}", self.id, id)
    }
    fn finish(&self, state: &'static str, error: Option<String>) {
        {
            let mut s = self.status.lock().unwrap();
            if !matches!(s.state, "connecting" | "connected") {
                return;
            }
            s.state = state;
            s.error = error.clone();
        }
        let why = error.unwrap_or_else(|| "Remote connection was disconnected".into());
        for (_, pending) in self.pending.lock().unwrap().drain() {
            let _ = pending.send(Err(why.clone()));
        }
        for (id, approval) in self.approvals.lock().unwrap().drain() {
            let _=self.out.send(json!({"method":"remote/notification","params":{"connectionId":self.id,"method":"approval/resolved","params":{"requestId":id,"threadId":approval.params["threadId"],"connectionId":self.id}}}));
        }
        let _ = self.stop.send(true);
        self.emit_status();
    }
    async fn send(&self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FRAME {
            return Err("Remote JSONL frame exceeds 32 MiB".into());
        }
        bytes.push(b'\n');
        let mut stopped = self.stop.subscribe();
        if *stopped.borrow() {
            return Err("Remote connection was disconnected".into());
        }
        let write = tokio::time::timeout(self.rpc_timeout, async {
            let mut guard = self.writer.lock().await;
            guard
                .as_mut()
                .ok_or("Remote stdin is closed")?
                .write_all(&bytes)
                .await
                .map_err(|_| "Could not write to the remote app-server".to_string())
        });
        tokio::select! {result=write=>result.map_err(|_|"Remote write timed out".to_string())?,_=stopped.changed()=>Err("Remote connection was disconnected".into())}
    }
    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        if !safe_text(method, 256) || method.starts_with("remote/") {
            return Err("Invalid or recursive remote RPC method".into());
        }
        let id = format!(
            "client:{}:{}",
            self.id,
            self.sequence.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        {
            let status = self.status.lock().unwrap();
            if !matches!(status.state, "connecting" | "connected") {
                return Err("Remote connection is not active".into());
            }
            self.pending.lock().unwrap().insert(id.clone(), tx);
        }
        if let Err(error) = self
            .send(&json!({"id":id,"method":method,"params":params}))
            .await
        {
            self.pending.lock().unwrap().remove(&id);
            self.finish("failed", Some(error.clone()));
            return Err(error);
        }
        let result = tokio::time::timeout(self.rpc_timeout, rx).await;
        self.pending.lock().unwrap().remove(&id);
        match result {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => Err("Remote response channel closed".into()),
            Err(_) => {
                let error =
                    "Remote RPC response timed out; connection closed to avoid orphaned work"
                        .to_string();
                self.finish("failed", Some(error.clone()));
                Err(error)
            }
        }
    }
    fn decorate(&self, value: &mut Value) {
        match value {
            Value::Object(object) => {
                for child in object.values_mut() {
                    self.decorate(child);
                }
                if object.contains_key("id") {
                    object.insert("connectionId".into(), json!(self.id));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.decorate(item)
                }
            }
            _ => {}
        }
    }
    async fn receive(&self, mut value: Value) -> Result<()> {
        if !value.is_object() {
            return Err("Remote stdout contained a non-object JSON frame".into());
        }
        if let Some(method) = value["method"].as_str().map(String::from) {
            if value.get("id").is_some() {
                if !method.ends_with("/requestApproval") {
                    self.send(&json!({"id":value["id"],"error":{"code":-32601,"message":"Unsupported remote client request"}})).await?;
                    return Ok(());
                }
                let remote_id = value["id"].clone();
                if !(remote_id.is_u64() || remote_id.is_i64() || remote_id.is_string()) {
                    return Err("Remote approval id is invalid".into());
                }
                let id = self.approval_id(&remote_id);
                let approval = Approval {
                    remote_id,
                    method: method.clone(),
                    params: value["params"].clone(),
                };
                if self
                    .approvals
                    .lock()
                    .unwrap()
                    .insert(id.clone(), approval)
                    .is_some()
                {
                    return Err("Remote server reused a pending approval id".into());
                }
                self.decorate(&mut value["params"]);
                value["params"]["connectionId"] = json!(self.id);
                let _=self.out.send(json!({"method":"remote/request","params":{"connectionId":self.id,"id":id,"method":method,"params":value["params"]}}));
            } else {
                if method == "approval/resolved" {
                    let id = self.approval_id(&value["params"]["requestId"]);
                    self.approvals.lock().unwrap().remove(&id);
                    value["params"]["requestId"] = json!(id);
                }
                self.decorate(&mut value["params"]);
                if value["params"].is_object() {
                    value["params"]["connectionId"] = json!(self.id);
                }
                let _=self.out.send(json!({"method":"remote/notification","params":{"connectionId":self.id,"method":method,"params":value["params"]}}));
            }
        } else if let Some(id) = value["id"].as_str() {
            if let Some(pending) = self.pending.lock().unwrap().remove(id) {
                let response = if value.get("error").is_some() {
                    Err(format!(
                        "Remote RPC error {}: {}",
                        value["error"]["code"],
                        value["error"]["message"]
                            .as_str()
                            .unwrap_or("Unknown remote error")
                    ))
                } else if value.get("result").is_some() {
                    Ok(value["result"].clone())
                } else {
                    Err("Remote response has neither result nor error".into())
                };
                let _ = pending.send(response);
            }
        } else {
            return Err("Remote JSONL frame has no valid method or response id".into());
        }
        Ok(())
    }
}

struct Inner {
    closed: AtomicBool,
    profiles: Profiles,
    connections: Mutex<HashMap<String, Arc<Connection>>>,
    gate: AsyncMutex<()>,
    out: UnboundedSender<Value>,
    ssh: PathBuf,
    keyscan: PathBuf,
    probes: Mutex<HashMap<String, Probe>>,
    handshake_timeout: Duration,
    rpc_timeout: Duration,
}
pub struct RemoteHub {
    inner: Arc<Inner>,
}
impl RemoteHub {
    pub fn new(out: UnboundedSender<Value>) -> Self {
        Self::configured(
            crate::config::home(),
            PathBuf::from("ssh"),
            out,
            Duration::from_secs(10),
            Duration::from_secs(30),
        )
    }
    fn configured(
        root: PathBuf,
        ssh: PathBuf,
        out: UnboundedSender<Value>,
        handshake_timeout: Duration,
        rpc_timeout: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                closed: AtomicBool::new(false),
                profiles: Profiles { root },
                connections: Default::default(),
                gate: AsyncMutex::new(()),
                out,
                keyscan: ssh.with_file_name("ssh-keyscan"),
                ssh,
                probes: Default::default(),
                handshake_timeout,
                rpc_timeout,
            }),
        }
    }
    fn connection(&self, p: &Value) -> Result<Arc<Connection>> {
        self.inner
            .connections
            .lock()
            .unwrap()
            .get(text(p, "connectionId")?)
            .cloned()
            .ok_or_else(|| "Remote connection not found".into())
    }
    pub async fn handle(&self, method: &str, p: &Value) -> Option<Result<Value>> {
        if !method.starts_with("remote/") {
            return None;
        }
        Some(self.dispatch(method, p).await)
    }
    async fn dispatch(&self, method: &str, p: &Value) -> Result<Value> {
        if self.inner.closed.load(Ordering::SeqCst)
            && !matches!(
                method,
                "remote/status" | "remote/disconnect" | "remote/profile/list"
            )
        {
            return Err("Remote service is closed".into());
        }
        match method {
            "remote/hostKey/probe" => self.probe_host(text(p, "profileId")?).await,
            "remote/hostKey/trust" => self.trust_host(p).await,
            "remote/profile/list"
            | "remote/profile/create"
            | "remote/profile/update"
            | "remote/profile/delete" => {
                let _guard = self.inner.gate.lock().await;
                if method != "remote/profile/list" && self.inner.closed.load(Ordering::SeqCst) {
                    return Err("Remote service is closed".into());
                }
                if matches!(method, "remote/profile/update" | "remote/profile/delete")
                    && self
                        .inner
                        .connections
                        .lock()
                        .unwrap()
                        .values()
                        .any(|c| Some(c.profile_id.as_str()) == p["id"].as_str() && c.active())
                {
                    return Err("Disconnect this profile before updating or deleting it".into());
                }
                let store = self.inner.profiles.clone();
                let method = method.to_owned();
                let p = p.clone();
                tokio::task::spawn_blocking(move || store.rpc(&method, &p))
                    .await
                    .map_err(|_| "Remote profile worker stopped")?
            }
            "remote/connect" => self.connect(text(p, "id")?).await,
            "remote/status" => {
                if p.get("connectionId").is_some() {
                    Ok(json!({"connection":self.connection(p)?.public()}))
                } else {
                    let mut data = self
                        .inner
                        .connections
                        .lock()
                        .unwrap()
                        .values()
                        .map(|c| c.public())
                        .collect::<Vec<_>>();
                    data.sort_by_key(|c| c["createdAt"].as_u64().unwrap_or(0));
                    Ok(json!({"data":data}))
                }
            }
            "remote/disconnect" => {
                let connection = self.connection(p)?;
                connection.finish("disconnected", None);
                connection.writer.lock().await.take();
                let job = connection.job.lock().unwrap().take();
                if let Some(job) = job {
                    let _ = tokio::time::timeout(Duration::from_secs(4), job).await;
                }
                Ok(json!({"connection":connection.public()}))
            }
            "remote/rpc" => {
                let connection = self.connection(p)?;
                if connection.status.lock().unwrap().state != "connected" {
                    return Err("Remote connection is not connected".into());
                }
                let method = text(p, "method")?;
                let mut result = connection
                    .request(
                        method,
                        p.get("params").cloned().unwrap_or_else(|| json!({})),
                    )
                    .await?;
                if method == "approval/list" {
                    for approval in result["data"].as_array_mut().into_iter().flatten() {
                        let remote_id = approval["id"].clone();
                        let local_id = connection.approval_id(&remote_id);
                        connection
                            .approvals
                            .lock()
                            .unwrap()
                            .entry(local_id.clone())
                            .or_insert(Approval {
                                remote_id,
                                method: approval["method"].as_str().unwrap_or("").into(),
                                params: approval["params"].clone(),
                            });
                        approval["id"] = json!(local_id);
                    }
                }
                connection.decorate(&mut result);
                if result.is_object() {
                    result["connectionId"] = json!(connection.id);
                }
                Ok(result)
            }
            "remote/approval/respond" => {
                let connection = self.connection(p)?;
                let id = text(p, "requestId")?;
                let decision = text(p, "decision")?;
                if !["accept", "acceptForSession", "decline", "cancel"].contains(&decision) {
                    return Err("Unsupported approval decision".into());
                }
                let approval = connection
                    .approvals
                    .lock()
                    .unwrap()
                    .remove(id)
                    .ok_or("Remote approval is no longer pending")?;
                if !approval.method.ends_with("/requestApproval") {
                    return Err("Remote request is not an approval".into());
                }
                if let Err(e) = connection
                    .send(&json!({"id":approval.remote_id,"result":{"decision":decision}}))
                    .await
                {
                    let _=connection.out.send(json!({"method":"remote/notification","params":{"connectionId":connection.id,"method":"approval/resolved","params":{"requestId":id,"threadId":approval.params["threadId"],"connectionId":connection.id}}}));
                    connection.finish("failed", Some(e.clone()));
                    return Err(e);
                }
                Ok(json!({"sent":true,"connectionId":connection.id,"requestId":id}))
            }
            _ => Err(format!("Unknown remote RPC: {method}")),
        }
    }
    async fn connect(&self, profile_id: &str) -> Result<Value> {
        let connection;
        {
            let _guard = self.inner.gate.lock().await;
            if self.inner.closed.load(Ordering::SeqCst) {
                return Err("Remote service is closed".into());
            }
            if let Some(existing) = self
                .inner
                .connections
                .lock()
                .unwrap()
                .values()
                .find(|c| c.profile_id == profile_id && c.active())
                .cloned()
            {
                let connected = existing.status.lock().unwrap().state == "connected";
                return if connected {
                    Ok(json!({"connection":existing.public()}))
                } else {
                    Err("This remote profile is still connecting".into())
                };
            }
            let profile = self.inner.profiles.get(profile_id)?;
            let (stop, mut stopped) = watch::channel(false);
            connection = Arc::new(Connection {
                id: id("rc"),
                profile_id: profile.id.clone(),
                created_at: now(),
                status: Mutex::new(Status {
                    state: "connecting",
                    error: None,
                    initialize: None,
                    stderr: Vec::new(),
                }),
                writer: AsyncMutex::new(None),
                pending: Default::default(),
                approvals: Default::default(),
                sequence: AtomicU64::new(1),
                stop,
                job: Default::default(),
                out: self.inner.out.clone(),
                rpc_timeout: self.inner.rpc_timeout,
            });
            {
                let mut connections = self.inner.connections.lock().unwrap();
                if connections.len() >= 100 {
                    let old = connections
                        .values()
                        .filter(|c| !c.active())
                        .min_by_key(|c| c.created_at)
                        .map(|c| c.id.clone())
                        .ok_or("Too many remote connections")?;
                    connections.remove(&old);
                }
                connections.insert(connection.id.clone(), connection.clone());
            }
            connection.emit_status();
            let mut command = Command::new(&self.inner.ssh);
            command.arg("-T");
            for option in [
                "BatchMode=yes",
                "PasswordAuthentication=no",
                "KbdInteractiveAuthentication=no",
                "NumberOfPasswordPrompts=0",
                "StrictHostKeyChecking=yes",
                "ConnectTimeout=10",
            ] {
                command.arg("-o").arg(option);
            }
            let known = self.inner.profiles.root.join("remote-known-hosts");
            match fs::symlink_metadata(&known) {
                Ok(metadata) => {
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        connection.finish("failed", Some("Unsafe App known_hosts file".into()));
                        return Err("Unsafe App known_hosts file".into());
                    }
                    let option = known
                        .canonicalize()
                        .map_err(|e| e.to_string())
                        .and_then(|absolute| known_hosts_command(&absolute));
                    match option {
                        Ok(option) => {
                            command.arg("-o").arg(format!("KnownHostsCommand={option}"));
                        }
                        Err(error) => {
                            connection.finish("failed", Some(error.clone()));
                            return Err(error);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    connection.finish(
                        "failed",
                        Some(format!("Cannot inspect App known_hosts: {e}")),
                    );
                    return Err(format!("Cannot inspect App known_hosts: {e}"));
                }
            }
            command.arg("-p").arg(profile.port.to_string());
            if let Some(user) = &profile.user {
                command.arg("-l").arg(user);
            }
            if let Some(identity) = &profile.identity_file {
                command.arg("-i").arg(identity);
            }
            command
                .arg("--")
                .arg(&profile.host)
                .arg(profile.remote_command());
            for (name, _) in std::env::vars_os() {
                let name_string = name.to_string_lossy();
                if name_string.starts_with("OPENAI_")
                    || name_string.starts_with("MCODEX_")
                    || name_string.starts_with("CODEX_")
                {
                    command.env_remove(name);
                }
            }
            command
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true);
            let mut child = match command.spawn() {
                Ok(child) => child,
                Err(e) => {
                    connection.finish("failed", Some(format!("Cannot start SSH: {e}")));
                    return Err(format!("Cannot start SSH: {e}"));
                }
            };
            *connection.writer.lock().await = child.stdin.take();
            let stdout = child.stdout.take().unwrap();
            let stderr = child.stderr.take().unwrap();
            let owned = connection.clone();
            let reader = tokio::spawn(async move {
                let result = read_frames(stdout, owned.clone()).await;
                owned.finish(
                    "failed",
                    Some(
                        result
                            .err()
                            .unwrap_or_else(|| "Remote stdout closed".into()),
                    ),
                );
            });
            let owned = connection.clone();
            let errors = tokio::spawn(async move {
                let mut stderr = stderr;
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk).await {
                    if n == 0 {
                        break;
                    }
                    let mut state = owned.status.lock().unwrap();
                    state.stderr.extend_from_slice(&chunk[..n]);
                    if state.stderr.len() > MAX_STDERR {
                        let excess = state.stderr.len() - MAX_STDERR;
                        state.stderr.drain(..excess);
                    }
                }
            });
            let owned = connection.clone();
            let job = tokio::spawn(async move {
                tokio::select! {exit=child.wait()=>{owned.finish("failed",Some(match exit{Ok(s)=>format!("SSH exited with {s}"),Err(e)=>format!("SSH wait failed: {e}")}));},_=stopped.changed()=>{let _=child.kill().await;let _=child.wait().await;}}
                owned.writer.lock().await.take();
                reader.abort();
                let _ = reader.await;
                let _ = tokio::time::timeout(Duration::from_secs(1), errors).await;
            });
            *connection.job.lock().unwrap() = Some(job);
        }
        let initialized = tokio::time::timeout(
            self.inner.handshake_timeout,
            connection.request(
                "initialize",
                json!({"clientInfo":{"name":"mcodex-ssh","version":env!("CARGO_PKG_VERSION")}}),
            ),
        )
        .await;
        let initialized = match initialized {
            Ok(Ok(value))
                if value["userAgent"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("mcodex/")) =>
            {
                value
            }
            Ok(Ok(_)) => {
                connection.finish(
                    "failed",
                    Some("Remote initialize did not identify a Rust mcodex backend".into()),
                );
                return Err("Remote initialize did not identify a Rust mcodex backend".into());
            }
            Ok(Err(error)) => {
                connection.finish("failed", Some(error.clone()));
                return Err(error);
            }
            Err(_) => {
                connection.finish(
                    "failed",
                    Some("Remote initialize handshake timed out".into()),
                );
                return Err("Remote initialize handshake timed out".into());
            }
        };
        {
            let mut status = connection.status.lock().unwrap();
            if status.state != "connecting" {
                return Err("Remote connection ended during initialize".into());
            }
            status.state = "connected";
            status.initialize = Some(initialized);
        }
        connection.emit_status();
        Ok(json!({"connection":connection.public()}))
    }
    pub async fn shutdown(&self) {
        let connections = {
            let _gate = self.inner.gate.lock().await;
            self.inner.closed.store(true, Ordering::SeqCst);
            self.inner
                .connections
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>()
        };
        for connection in &connections {
            connection.finish("disconnected", None);
        }
        futures_util::future::join_all(connections.into_iter().map(|connection| async move {
            connection.writer.lock().await.take();
            let job = connection.job.lock().unwrap().take();
            if let Some(job) = job {
                let _ = tokio::time::timeout(Duration::from_secs(4), job).await;
            }
        }))
        .await;
    }
    async fn probe_host(&self, profile_id: &str) -> Result<Value> {
        let profile = self.inner.profiles.get(profile_id)?;
        let mut command = Command::new(&self.inner.keyscan);
        command
            .args([
                "-T",
                "5",
                "-p",
                &profile.port.to_string(),
                "--",
                &profile.host,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| format!("Cannot start ssh-keyscan: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let output = tokio::time::timeout(Duration::from_secs(8), async {
            let (output, _) = tokio::try_join!(
                limited_read(stdout, 128 * 1024),
                limited_read(stderr, MAX_STDERR)
            )?;
            let exit = child.wait().await.map_err(|e| e.to_string())?;
            if !exit.success() {
                return Err("SSH host-key scan failed; confirm the host and port".into());
            }
            Ok::<Vec<u8>, String>(output)
        })
        .await
        .map_err(|_| "SSH host-key scan timed out".to_string())??;
        let source = String::from_utf8(output).map_err(|_| "ssh-keyscan returned invalid UTF-8")?;
        let mut keys = Vec::new();
        for line in source
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        {
            let key = host_key(&profile, line)?;
            if !keys.iter().any(|k: &HostKey| k.line == key.line) {
                keys.push(key);
            }
            if keys.len() > 16 {
                return Err("Host-key scan returned too many keys".into());
            }
        }
        if keys.is_empty() {
            return Err("No SSH host keys were found".into());
        }
        keys.sort_by(|a, b| a.line.cmp(&b.line));
        let _guard = self.inner.gate.lock().await;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("Remote service is closed".into());
        }
        if self.inner.profiles.get(profile_id)? != profile {
            return Err("Remote profile changed during the host-key scan; probe again".into());
        }
        let expires_at = now() + 5 * 60 * 1000;
        let result = json!({"profileId":profile_id,"host":profile.host,"port":profile.port,"verified":false,"keys":keys.iter().map(|k|k.line.clone()).collect::<Vec<_>>(),"fingerprints":keys.iter().map(|k|k.fingerprint.clone()).collect::<Vec<_>>(),"expiresAt":expires_at});
        self.inner.probes.lock().unwrap().insert(
            profile_id.into(),
            Probe {
                profile,
                keys,
                expires_at,
            },
        );
        Ok(result)
    }
    async fn trust_host(&self, p: &Value) -> Result<Value> {
        let _guard = self.inner.gate.lock().await;
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("Remote service is closed".into());
        }
        known_hosts_command(&self.inner.profiles.root.join("remote-known-hosts"))?;
        let profile_id = text(p, "profileId")?;
        let probe = self
            .inner
            .probes
            .lock()
            .unwrap()
            .get(profile_id)
            .cloned()
            .filter(|probe| probe.expires_at >= now())
            .ok_or(
                "Probe this profile's SSH host key first; previous probe is missing or expired",
            )?;
        let lines = p["keys"]
            .as_array()
            .filter(|keys| !keys.is_empty() && keys.len() <= 16)
            .ok_or("keys must contain 1 to 16 previously displayed full host keys")?;
        let fingerprints = p["fingerprints"]
            .as_array()
            .filter(|fingerprints| fingerprints.len() == lines.len())
            .ok_or("Confirm one displayed fingerprint for each host key")?;
        let mut keys = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (line, fingerprint) in lines.iter().zip(fingerprints) {
            let line = line.as_str().ok_or("Host keys must be strings")?;
            let key = host_key(&probe.profile, line)?;
            if !seen.insert(key.line.clone())
                || fingerprint.as_str() != Some(key.fingerprint.as_str())
                || !probe
                    .keys
                    .iter()
                    .any(|p| p.line == key.line && p.fingerprint == key.fingerprint)
            {
                return Err(
                    "Trusted host key/fingerprint does not match the displayed probe".into(),
                );
            }
            keys.push(key);
        }
        self.inner.profiles.transaction(|state|{
            if state.profiles.iter().find(|p|p.id==profile_id)!=Some(&probe.profile){return Err("Remote profile changed since the probe; verify it again".into());}
            let file=self.inner.profiles.root.join("remote-known-hosts");
            if fs::symlink_metadata(&file).is_ok_and(|m|!m.is_file()||m.file_type().is_symlink()){return Err("Unsafe App known_hosts file".into());}
            let old=match fs::read(&file){Ok(bytes) if bytes.len()<=1024*1024=>String::from_utf8(bytes).map_err(|_|"Invalid App known_hosts encoding")?,Ok(_)=>return Err("App known_hosts exceeds 1 MiB".into()),Err(e) if e.kind()==std::io::ErrorKind::NotFound=>String::new(),Err(e)=>return Err(format!("Cannot read App known_hosts: {e}"))};
            let token=host_token(&probe.profile);let mut contents=old.lines().filter(|line|line.split_whitespace().next()!=Some(token.as_str())).map(|s|format!("{s}\n")).collect::<String>();for key in &keys{contents.push_str(&key.line);contents.push('\n');}if contents.len()>1024*1024{return Err("App known_hosts exceeds 1 MiB".into());}
            let tmp=self.inner.profiles.root.join(format!(".known-hosts-{}.tmp",id("trust")));let write=(||{let mut options=OpenOptions::new();options.create_new(true).write(true);#[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}let mut f=options.open(&tmp).map_err(|e|e.to_string())?;f.write_all(contents.as_bytes()).and_then(|_|f.sync_all()).map_err(|e|e.to_string())?;fs::rename(&tmp,&file).map_err(|e|e.to_string())?;File::open(&self.inner.profiles.root).and_then(|f|f.sync_all()).map_err(|e|e.to_string())})();if write.is_err(){let _=fs::remove_file(&tmp);}write?;
            Ok((json!({"trusted":true,"profileId":profile_id,"fingerprints":keys.iter().map(|k|k.fingerprint.clone()).collect::<Vec<_>>()}),false))
        })
    }
}

async fn read_frames<R: AsyncRead + Unpin>(reader: R, connection: Arc<Connection>) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut frame = Vec::new();
    loop {
        let bytes = reader
            .fill_buf()
            .await
            .map_err(|_| "Could not read remote stdout")?;
        if bytes.is_empty() {
            return Err(if frame.is_empty() {
                "Remote app-server disconnected"
            } else {
                "Remote stdout ended with an incomplete JSONL frame"
            }
            .into());
        }
        let newline = bytes.iter().position(|b| *b == b'\n');
        let n = newline.map(|i| i + 1).unwrap_or(bytes.len());
        if frame.len() + n > MAX_FRAME + 2 {
            return Err("Remote JSONL frame exceeds 32 MiB".into());
        }
        frame.extend_from_slice(&bytes[..n]);
        reader.consume(n);
        if newline.is_some() {
            frame.pop();
            if frame.last() == Some(&b'\r') {
                frame.pop();
            }
            if frame.len() > MAX_FRAME {
                return Err("Remote JSONL frame exceeds 32 MiB".into());
            }
            if !frame.is_empty() {
                let value = serde_json::from_slice(&frame).map_err(|_| {
                    "Remote stdout contained invalid JSON; use mcodex app-server over stdio"
                })?;
                connection.receive(value).await?;
            }
            frame.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    struct Fixture {
        root: PathBuf,
        store: PathBuf,
        remote: PathBuf,
        cwd: PathBuf,
        ssh: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("mcodex remote ' quoted {}", id("test")));
            fs::create_dir_all(&root).unwrap();
            let store = root.join("local store ' config");
            let remote = root.join("remote home");
            let cwd = root.join("selected cwd ' literal");
            for path in [&store, &remote, &cwd] {
                fs::create_dir_all(path).unwrap();
            }
            let ssh = root.join("fake-ssh");
            let f = Self {
                root,
                store,
                remote,
                cwd,
                ssh,
            };
            f.real_ssh(None);
            f
        }
        fn executable(&self, path: &Path, source: &str) {
            fs::write(path, source).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        fn real_ssh(&self, base: Option<&str>) {
            let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/mcodex");
            assert!(binary.is_file(),"Run cargo build --no-default-features --bin mcodex before remote integration tests");
            fs::write(
                self.remote.join("auth.json"),
                br#"{"auth_mode":"apikey","OPENAI_API_KEY":"remote-fixture-key"}"#,
            )
            .unwrap();
            let config = json!({"home":self.remote,"cwd":self.cwd,"binary":binary,"base":base,"log":self.root.join("ssh-argv.jsonl")});
            let script = format!(
                r#"#!/usr/bin/env python3
import os,sys,json,shlex
c=json.loads({config:?})
a=sys.argv[1:]
assert '-T' in a and 'BatchMode=yes' in a and 'StrictHostKeyChecking=yes' in a
assert not any(k.startswith(('OPENAI_','MCODEX_','CODEX_')) for k in os.environ)
assert a[-2]=='fixture.invalid'
cmd=shlex.split(a[-1])
assert cmd[0]==c['binary'] and cmd[-1]==c['cwd']
with open(c['log'],'a') as f:f.write(json.dumps(a)+'\n')
# These are real app-server processes, but each fixture needs only two workers.
# Runtime::new otherwise starts one worker per logical CPU for every concurrent
# test child (15 total threads on the 12-core host versus 5 with this bound).
e={{'PATH':os.environ.get('PATH','/usr/bin:/bin'),'HOME':c['home'],'MCODEX_HOME':c['home'],'MCODEX_NO_CODEX_IMPORT':'1','LANG':'en_US.UTF-8','TOKIO_WORKER_THREADS':'2'}}
if c['base']:e['OPENAI_BASE_URL']=c['base']
os.execve('/bin/sh',['sh','-c',a[-1]],e)
"#,
                config = config.to_string()
            );
            self.executable(&self.ssh, &script);
        }
        fn hub(&self) -> (Arc<RemoteHub>, UnboundedReceiver<Value>) {
            // Match the real startup allowance: concurrent Rust/Python children
            // can take longer than three seconds in the complete suite.
            self.hub_with_timeouts(Duration::from_secs(10), Duration::from_secs(15))
        }
        fn hub_with_timeouts(
            &self,
            handshake: Duration,
            rpc: Duration,
        ) -> (Arc<RemoteHub>, UnboundedReceiver<Value>) {
            let (out, rx) = unbounded_channel();
            (
                Arc::new(RemoteHub::configured(
                    self.store.clone(),
                    self.ssh.clone(),
                    out,
                    handshake,
                    rpc,
                )),
                rx,
            )
        }
        async fn profile(&self, hub: &RemoteHub) -> String {
            let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/mcodex");
            rpc(hub,"remote/profile/create",json!({"name":"Selected remote","host":"fixture.invalid","user":"fixture","port":2222,"identityFile":self.root.join("never-read-key"),"remoteCwd":self.cwd,"agentPath":binary})).await["profile"]["id"].as_str().unwrap().into()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    async fn rpc(hub: &RemoteHub, method: &str, p: Value) -> Value {
        tokio::time::timeout(Duration::from_secs(20), hub.handle(method, &p))
            .await
            .expect("bounded remote test RPC")
            .unwrap()
            .unwrap()
    }
    async fn event(rx: &mut UnboundedReceiver<Value>, method: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let event = rx.recv().await.unwrap();
                if event["method"] == method {
                    return event;
                }
            }
        })
        .await
        .expect("bounded remote event")
    }
    fn sample_key(profile: &Profile) -> HostKey {
        let mut raw = Vec::new();
        for bytes in [b"ssh-ed25519".as_slice(), &[7u8; 32]] {
            raw.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
            raw.extend_from_slice(bytes);
        }
        host_key(
            profile,
            &format!(
                "{} ssh-ed25519 {}",
                host_token(profile),
                STANDARD.encode(raw)
            ),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn profiles_are_private_durable_and_reject_option_or_shell_injection() {
        let f = Fixture::new();
        let (hub, _) = f.hub();
        let profile = f.profile(&hub).await;
        let loaded = Profiles {
            root: f.store.clone(),
        }
        .get(&profile)
        .unwrap();
        assert_eq!(loaded.host, "fixture.invalid");
        assert_eq!(loaded.port, 2222);
        assert!(!f.root.join("never-read-key").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(f.store.join("remote-profiles.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        for p in [
            json!({"id":profile,"host":"-oProxyCommand=bad"}),
            json!({"id":profile,"host":"host;touch bad"}),
            json!({"id":profile,"remoteCwd":"relative"}),
            json!({"id":profile,"identityFile":"relative-key"}),
            json!({"id":profile,"port":0}),
            json!({"id":profile,"agentPath":"-bad"}),
        ] {
            assert!(hub
                .handle("remote/profile/update", &p)
                .await
                .unwrap()
                .is_err());
        }
        rpc(
            &hub,
            "remote/profile/update",
            json!({"id":profile,"name":"Renamed"}),
        )
        .await;
        assert_eq!(
            rpc(&hub, "remote/profile/list", json!({})).await["data"][0]["name"],
            "Renamed"
        );
        let command = format!(
            "/usr/bin/printf '%s' {}",
            quote("a ' ; $(touch forbidden) %h $HOME")
        );
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(&f.root)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "a ' ; $(touch forbidden) %h $HOME"
        );
        assert!(!f.root.join("forbidden").exists());
        rpc(&hub, "remote/profile/delete", json!({"id":profile})).await;
        assert_eq!(
            rpc(&hub, "remote/profile/list", json!({})).await["data"],
            json!([])
        );
        hub.shutdown().await;
    }

    #[tokio::test]
    async fn actual_rust_remote_process_executes_and_reconnects_without_losing_sessions() {
        let f = Fixture::new();
        let (hub, _rx) = f.hub();
        let profile = f.profile(&hub).await;
        let connection =
            rpc(&hub, "remote/connect", json!({"id":profile})).await["connection"].clone();
        let cid = connection["id"].as_str().unwrap();
        assert_eq!(connection["state"], "connected");
        assert!(connection["initialize"]["userAgent"]
            .as_str()
            .unwrap()
            .starts_with("mcodex/"));
        assert!(hub
            .handle("remote/profile/delete", &json!({"id":profile}))
            .await
            .unwrap()
            .is_err());
        let started = rpc(
            &hub,
            "remote/rpc",
            json!({"connectionId":cid,"method":"thread/start","params":{"mode":"codex"}}),
        )
        .await;
        let tid = started["thread"]["id"].as_str().unwrap();
        assert_eq!(started["thread"]["connectionId"], cid);
        let exec=rpc(&hub,"remote/rpc",json!({"connectionId":cid,"method":"command/exec","params":{"cwd":f.cwd,"command":"printf REMOTE_PROCESS_REAL","sandboxPolicy":{"type":"dangerFullAccess"}}})).await;
        assert!(exec["output"]
            .as_str()
            .unwrap()
            .contains("REMOTE_PROCESS_REAL"));
        let rename = "Remote persisted session";
        rpc(&hub,"remote/rpc",json!({"connectionId":cid,"method":"thread/name/set","params":{"threadId":tid,"name":rename}})).await;
        let files=rpc(&hub,"remote/rpc",json!({"connectionId":cid,"method":"files/import","params":{"name":"remote.txt","dataBase64":STANDARD.encode("Remote full text 中文")}})).await;
        assert_eq!(files["attachment"]["connectionId"], cid);
        assert_eq!(
            rpc(&hub, "remote/disconnect", json!({"connectionId":cid})).await["connection"]
                ["state"],
            "disconnected"
        );
        assert!(hub
            .handle(
                "remote/rpc",
                &json!({"connectionId":cid,"method":"thread/list"})
            )
            .await
            .unwrap()
            .is_err());
        let second = rpc(&hub, "remote/connect", json!({"id":profile})).await["connection"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(second, cid);
        let restored = rpc(
            &hub,
            "remote/rpc",
            json!({"connectionId":second,"method":"thread/resume","params":{"threadId":tid}}),
        )
        .await;
        assert_eq!(restored["thread"]["name"], rename);
        assert_eq!(restored["thread"]["connectionId"], second);
        let argv = fs::read_to_string(f.root.join("ssh-argv.jsonl")).unwrap();
        assert!(argv.contains("StrictHostKeyChecking=yes"));
        assert!(argv.contains("PasswordAuthentication=no"));
        hub.shutdown().await;
    }

    #[tokio::test]
    async fn disconnect_fails_in_flight_requests_and_invalid_handshakes_do_not_connect() {
        let f = Fixture::new();
        let (hub, _) = f.hub();
        let profile = f.profile(&hub).await;
        let connected = rpc(&hub, "remote/connect", json!({"id":profile})).await;
        let cid = connected["connection"]["id"].as_str().unwrap().to_owned();
        let owned = hub.clone();
        let selected = cid.clone();
        let cwd = f.cwd.clone();
        let pending = tokio::spawn(async move {
            owned.handle("remote/rpc",&json!({"connectionId":selected,"method":"command/exec","params":{"command":"sleep 2","cwd":cwd,"sandboxPolicy":{"type":"dangerFullAccess"}}})).await.unwrap()
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        rpc(&hub, "remote/disconnect", json!({"connectionId":cid})).await;
        assert!(tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        hub.shutdown().await;
        for (source, timeout) in [
            ("#!/bin/sh\nprintf 'not JSON\\n'\n", Duration::from_secs(10)),
            ("#!/bin/sh\nexec sleep 10\n", Duration::from_millis(250)),
            (
                "#!/bin/sh\nprintf 'ssh-fixture-error' >&2\nexit 2\n",
                Duration::from_secs(10),
            ),
        ] {
            let f = Fixture::new();
            f.executable(&f.ssh, source);
            let (hub, _) = f.hub_with_timeouts(timeout, Duration::from_secs(15));
            let profile = f.profile(&hub).await;
            let error = hub
                .handle("remote/connect", &json!({"id":profile}))
                .await
                .unwrap()
                .unwrap_err();
            if source.contains("exec sleep") {
                assert!(error.contains("initialize handshake timed out"), "{error}");
            }
            let status = rpc(&hub, "remote/status", json!({})).await;
            assert_eq!(status["data"][0]["state"], "failed");
            hub.shutdown().await;
        }
    }

    #[tokio::test]
    async fn host_keys_require_matching_probe_and_explicit_fingerprints_and_keep_paths_literal() {
        let f = Fixture::new();
        let (hub, _) = f.hub();
        let profile_id = f.profile(&hub).await;
        let profile = hub.inner.profiles.get(&profile_id).unwrap();
        let key = sample_key(&profile);
        let scanner = f.ssh.with_file_name("ssh-keyscan");
        f.executable(
            &scanner,
            &format!("#!/bin/sh\nprintf '%s\\n' {}\n", quote(&key.line)),
        );
        assert!(hub
            .handle(
                "remote/hostKey/trust",
                &json!({"profileId":profile_id,"keys":[key.line],"fingerprints":[key.fingerprint]})
            )
            .await
            .unwrap()
            .is_err());
        let probe = rpc(
            &hub,
            "remote/hostKey/probe",
            json!({"profileId":profile_id}),
        )
        .await;
        assert_eq!(probe["verified"], false);
        assert_eq!(probe["keys"], json!([key.line]));
        assert!(hub.handle("remote/hostKey/trust",&json!({"profileId":profile_id,"keys":probe["keys"],"fingerprints":["SHA256:wrong"]})).await.unwrap().is_err());
        rpc(&hub,"remote/hostKey/trust",json!({"profileId":profile_id,"keys":probe["keys"],"fingerprints":probe["fingerprints"]})).await;
        let known = f.store.join("remote-known-hosts");
        assert_eq!(
            fs::read_to_string(&known).unwrap(),
            format!("{}\n", key.line)
        );
        let command = known_hosts_command(&known).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, format!("{}\n", key.line).as_bytes());
        assert!(known_hosts_command(Path::new("/tmp/${HOME}/known_hosts")).is_err());
        assert!(known_hosts_command(Path::new("/tmp/%h path ' quoted"))
            .unwrap()
            .contains("%%h"));
        let connection = rpc(&hub, "remote/connect", json!({"id":profile_id})).await;
        let arguments = fs::read_to_string(f.root.join("ssh-argv.jsonl")).unwrap();
        assert!(arguments.contains("KnownHostsCommand="));
        assert!(!arguments.contains("UserKnownHostsFile="));
        assert!(!arguments.contains("GlobalKnownHostsFile="));
        rpc(
            &hub,
            "remote/disconnect",
            json!({"connectionId":connection["connection"]["id"]}),
        )
        .await;
        rpc(
            &hub,
            "remote/profile/update",
            json!({"id":profile_id,"port":2223}),
        )
        .await;
        assert!(hub.handle("remote/hostKey/trust",&json!({"profileId":profile_id,"keys":probe["keys"],"fingerprints":probe["fingerprints"]})).await.unwrap().is_err());
        hub.shutdown().await;
    }

    #[tokio::test]
    async fn real_agent_approval_is_remapped_answered_and_executes_on_the_remote_process() {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for round in 0..2 {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(10), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let (header_end, length) = loop {
                    let mut chunk = [0u8; 4096];
                    let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
                        .await
                        .unwrap()
                        .unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&chunk[..n]);
                    assert!(bytes.len() < 1024 * 1024);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .filter_map(|l| l.split_once(':'))
                            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .unwrap()
                            .1
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break (end, length);
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                assert!(headers.starts_with("POST /responses "));
                assert!(headers
                    .to_lowercase()
                    .contains("authorization: bearer remote-fixture-key"));
                requests.push(
                    serde_json::from_slice::<Value>(
                        &bytes[header_end + 4..header_end + 4 + length],
                    )
                    .unwrap(),
                );
                let item = if round == 0 {
                    json!({"type":"function_call","id":"remote_function","call_id":"remote_call","name":"shell","arguments":json!({"command":["/usr/bin/printf","REMOTE_APPROVAL_EXECUTED"]}).to_string()})
                } else {
                    json!({"type":"message","id":"remote_answer","role":"assistant","content":[{"type":"output_text","text":"Remote approved execution completed"}]})
                };
                let body = format!(
                    "data: {}\r\n\r\ndata: {}\r\n\r\n",
                    json!({"type":"response.output_item.done","item":item}),
                    json!({"type":"response.completed","response":{"status":"completed","output":[item]}})
                );
                let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        let f = Fixture::new();
        f.real_ssh(Some(&base));
        let (hub, mut notifications) = f.hub();
        let profile = f.profile(&hub).await;
        let connected = rpc(&hub, "remote/connect", json!({"id":profile})).await;
        let cid = connected["connection"]["id"].as_str().unwrap();
        let started = rpc(
            &hub,
            "remote/rpc",
            json!({"connectionId":cid,"method":"thread/start","params":{"mode":"codex"}}),
        )
        .await;
        let tid = started["thread"]["id"].as_str().unwrap();
        rpc(&hub,"remote/rpc",json!({"connectionId":cid,"method":"turn/start","params":{"threadId":tid,"input":[{"type":"text","text":"Use the isolated remote fixture command"}]}})).await;
        let approval = event(&mut notifications, "remote/request").await;
        let request_id = approval["params"]["id"].as_str().unwrap();
        assert!(request_id.starts_with(&format!("remote:{cid}:")));
        assert_eq!(approval["params"]["connectionId"], cid);
        assert_eq!(approval["params"]["params"]["threadId"], tid);
        let pending = rpc(
            &hub,
            "remote/rpc",
            json!({"connectionId":cid,"method":"approval/list","params":{"threadId":tid}}),
        )
        .await;
        assert_eq!(pending["data"][0]["id"], request_id);
        rpc(
            &hub,
            "remote/approval/respond",
            json!({"connectionId":cid,"requestId":request_id,"decision":"accept"}),
        )
        .await;
        let mut resolved = false;
        let mut complete = false;
        tokio::time::timeout(Duration::from_secs(10), async {
            while !resolved || !complete {
                let note = notifications.recv().await.unwrap();
                if note["method"] != "remote/notification" {
                    continue;
                }
                if note["params"]["method"] == "approval/resolved" {
                    assert_eq!(note["params"]["params"]["requestId"], request_id);
                    resolved = true;
                }
                if note["params"]["method"] == "turn/completed" {
                    assert_eq!(note["params"]["params"]["turn"]["status"], "completed");
                    complete = true;
                }
            }
        })
        .await
        .unwrap();
        let requests = server.await.unwrap();
        assert!(requests[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output"
                && item["output"]
                    .as_str()
                    .unwrap_or("")
                    .contains("REMOTE_APPROVAL_EXECUTED")));
        let restored = rpc(
            &hub,
            "remote/rpc",
            json!({"connectionId":cid,"method":"thread/resume","params":{"threadId":tid}}),
        )
        .await;
        assert!(restored["thread"]["turns"]
            .to_string()
            .contains("Remote approved execution completed"));
        hub.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_blocks_late_connect_and_kills_an_inflight_handshake() {
        let f = Fixture::new();
        let pidfile = f.root.join("pending.pid");
        // Wait for the real initialize request before declaring readiness. The
        // whole Rust suite starts many subprocesses, so a two-second Python
        // startup deadline tests machine load rather than shutdown semantics.
        // Keep this handshake pending past the startup budget; only shutdown
        // may end it successfully for this regression.
        f.executable(&f.ssh,&format!("#!/usr/bin/env python3\nimport json,os,sys,time\nr=json.loads(sys.stdin.readline())\nassert r['method']=='initialize'\np={:?}\nwith open(p+'.tmp','w') as ready: ready.write(str(os.getpid()))\nos.replace(p+'.tmp',p)\ntime.sleep(60)\n",pidfile.to_str().unwrap()));
        let (out, _) = unbounded_channel();
        let hub = Arc::new(RemoteHub::configured(
            f.store.clone(),
            f.ssh.clone(),
            out,
            Duration::from_secs(20),
            Duration::from_secs(25),
        ));
        let profile = f.profile(&hub).await;
        let owned = hub.clone();
        let p = profile.clone();
        let connect = tokio::spawn(async move {
            owned
                .handle("remote/connect", &json!({"id":p}))
                .await
                .unwrap()
        });
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match fs::read_to_string(&pidfile) {
                    Ok(pid) => {
                        return pid
                            .trim()
                            .parse::<u32>()
                            .map_err(|e| format!("Invalid ready PID: {e}"))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(format!("Could not read ready PID: {e}")),
                }
                if connect.is_finished() {
                    return Err(
                        "Connect ended before the fake SSH process received initialize".into(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let pid = match ready {
            Ok(Ok(pid)) => pid,
            other => {
                let status = hub
                    .inner
                    .connections
                    .lock()
                    .unwrap()
                    .values()
                    .map(|c| c.public())
                    .collect::<Vec<_>>();
                hub.shutdown().await;
                let connection_result = tokio::time::timeout(Duration::from_secs(2), connect).await;
                panic!("Fake SSH did not enter its pending initialize phase: {other:?}; status={status:?}; connect={connection_result:?}");
            }
        };
        assert!(
            hub.inner
                .connections
                .lock()
                .unwrap()
                .values()
                .all(|connection| connection.status.lock().unwrap().state == "connecting"),
            "Fixture must still be inside initialize before shutdown"
        );
        hub.shutdown().await;
        assert!(tokio::time::timeout(Duration::from_secs(2), connect)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(hub
            .handle("remote/connect", &json!({"id":profile}))
            .await
            .unwrap()
            .unwrap_err()
            .contains("closed"));
        assert!(hub
            .handle(
                "remote/profile/create",
                &json!({"name":"Late","host":"fixture.invalid","remoteCwd":"/tmp"})
            )
            .await
            .unwrap()
            .unwrap_err()
            .contains("closed"));
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", &pid.to_string()])
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "shutdown left the fake SSH process alive"
        );
    }

    #[tokio::test]
    async fn frame_parser_rejects_malformed_or_oversized_stdout_without_unbounded_buffer() {
        let (out, _) = unbounded_channel();
        let (stop, _) = watch::channel(false);
        let connection = Arc::new(Connection {
            id: "rc_fixture".into(),
            profile_id: "rp_fixture".into(),
            created_at: now(),
            status: Mutex::new(Status {
                state: "connected",
                error: None,
                initialize: None,
                stderr: Vec::new(),
            }),
            writer: AsyncMutex::new(None),
            pending: Default::default(),
            approvals: Default::default(),
            sequence: AtomicU64::new(1),
            stop,
            job: Default::default(),
            out,
            rpc_timeout: Duration::from_secs(1),
        });
        assert!(read_frames(
            std::io::Cursor::new(b"not-json\n".to_vec()),
            connection.clone()
        )
        .await
        .unwrap_err()
        .contains("invalid JSON"));
        assert!(read_frames(
            std::io::Cursor::new(vec![b' '; MAX_FRAME + 3]),
            connection.clone()
        )
        .await
        .unwrap_err()
        .contains("32 MiB"));
        assert!(read_frames(
            std::io::Cursor::new(b"{\"id\":\"x\",\"result\":{}}".to_vec()),
            connection
        )
        .await
        .unwrap_err()
        .contains("incomplete"));
    }
}
