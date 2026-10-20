//! API-key-only Images API jobs. Cancellation stops local waiting, not provider billing.
//! Prompts and results are local; keys and provider response bodies never enter public errors.

use crate::auth::Auth;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{mpsc::UnboundedSender, Notify},
    task::JoinHandle,
};

type Result<T> = std::result::Result<T, String>;
type Credentials = Arc<dyn Fn() -> Result<(Auth, String)> + Send + Sync>;
type ReadImage = Arc<dyn Fn(&str) -> Result<(Value, Vec<u8>)> + Send + Sync>;
type SaveImage = Arc<dyn Fn(&str, &str, &[u8]) -> Result<Value> + Send + Sync>;
const DEFAULT_MODEL: &str = "gpt-image-2.5-sunburst";
const MODELS: [&str; 10] = [
    "gpt-image-2.5-sunburst",
    "gpt-image-2.5-flare",
    "gpt-image-2.5-sunburst-2026-09-08",
    "gpt-image-2.5-flare-2026-09-08",
    "gpt-image-2",
    "gpt-image-2-2026-04-21",
    "gpt-image-1.5",
    "gpt-image-1",
    "gpt-image-1-mini",
    "chatgpt-image-latest",
];
const MAX_IMAGE: usize = 10 * 1024 * 1024;
const MAX_RESPONSE: usize = 64 * 1024 * 1024;
const MAX_STORE: usize = 64 * 1024 * 1024;
const MAX_JOBS: usize = 1000;
const CANCEL_NOTE: &str = "Local waiting was cancelled. The provider may still finish the request and charge for it. This app will not save a late result or retry automatically.";
const CREDENTIALS_CHANGED: &str = "Image credentials changed before the request was sent. Start a new request with the current account.";
static STORE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone)]
struct Options {
    root: PathBuf,
    base: String,
    timeout: Duration,
}
#[derive(Clone)]
struct Store {
    root: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Job {
    id: String,
    kind: String,
    status: String,
    prompt: String,
    model: String,
    size: String,
    quality: String,
    background: String,
    output_format: String,
    n: u8,
    image_ids: Vec<String>,
    mask_id: Option<String>,
    created_at: u64,
    updated_at: u64,
    #[serde(default)]
    revision: u64,
    attachment_ids: Vec<String>,
    attachments: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancel_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Value>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    jobs: BTreeMap<String, Job>,
}
struct Running {
    cancelled: Arc<AtomicBool>,
    changed: Arc<Notify>,
    generation: u64,
}
#[derive(Default)]
struct Registry {
    generation: u64,
    closed: bool,
    running: HashMap<String, Running>,
    workers: Vec<JoinHandle<()>>,
}
struct Inner {
    store: Store,
    options: Options,
    registry: Mutex<Registry>,
    startup_error: Option<String>,
    credentials: Credentials,
    read_image: ReadImage,
    save_image: SaveImage,
    http: reqwest::Client,
    out: UnboundedSender<Value>,
}
pub struct Images {
    inner: Arc<Inner>,
}
struct Decoded {
    bytes: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
    alpha: bool,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn active(status: &str) -> bool {
    matches!(status, "queued" | "running")
}
fn valid_job_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("img_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn valid_attachment(id: &str) -> bool {
    id.len() == 36 && id.starts_with("att_") && id[4..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn status_valid(status: &str) -> bool {
    matches!(
        status,
        "queued" | "running" | "completed" | "failed" | "cancelled" | "interrupted"
    )
}
fn job_value(job: &Job) -> Value {
    serde_json::to_value(job).expect("image job is JSON-safe")
}
fn notify(inner: &Inner, job: &Job) {
    let _ = inner
        .out
        .send(json!({"method":"images/job/updated","params":{"job":job_value(job)}}));
}
fn string<'a>(p: &'a Value, name: &str, default: &'a str) -> Result<&'a str> {
    match p.get(name) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(v)) => Ok(v),
        _ => Err(format!("{name} must be a string")),
    }
}
fn model_options() -> Value {
    let mut options = serde_json::Map::new();
    for model in MODELS {
        let latest = model.starts_with("gpt-image-2.5-");
        let second = model == "gpt-image-2" || model == "gpt-image-2-2026-04-21";
        options.insert(model.into(),json!({"qualities":if latest{vec!["auto","low","medium","high","xhigh","max"]}else{vec!["auto","low","medium","high"]},"customSizes":latest||second,"transparentBackground":if second{"preview"}else{"supported"},"outputFormats":["png","jpeg","webp"]}));
    }
    Value::Object(options)
}

impl Store {
    fn locked<T>(&self, f: impl FnOnce(&mut Document) -> Result<(T, bool)>) -> Result<T> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "Image history lock is unavailable")?;
        private_dir(&self.root)?;
        let lock_path = self.root.join(".lock");
        safe_file(&lock_path, true)?;
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|_| "Cannot open image history lock")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => return Err("Image history is busy".into()),
            }
        }
        let path = self.root.join("jobs.json");
        safe_file(&path, true)?;
        let mut doc = if path.exists() {
            let mut bytes = Vec::new();
            File::open(&path)
                .map_err(|_| "Cannot open image history")?
                .take(MAX_STORE as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| "Cannot read image history")?;
            if bytes.len() > MAX_STORE {
                return Err("Image history exceeds its storage limit".into());
            }
            let parsed: Document = serde_json::from_slice(&bytes)
                .map_err(|_| "Image history is damaged; it was not overwritten")?;
            if parsed.version != 1
                || parsed.jobs.len() > MAX_JOBS
                || parsed.jobs.iter().any(|(id, j)| {
                    id != &j.id
                        || !valid_job_id(id)
                        || !status_valid(&j.status)
                        || j.attachments.len() > 4
                        || j.attachment_ids.iter().any(|v| !valid_attachment(v))
                })
            {
                return Err("Image history is damaged; it was not overwritten".into());
            }
            parsed
        } else {
            Document {
                version: 1,
                ..Default::default()
            }
        };
        let (result, changed) = f(&mut doc)?;
        if changed {
            let bytes = serde_json::to_vec(&doc).map_err(|_| "Cannot encode image history")?;
            if bytes.len() > MAX_STORE {
                return Err("Image history exceeds its storage limit".into());
            }
            let temp = self
                .root
                .join(format!(".jobs-{:032x}.tmp", rand::random::<u128>()));
            let saved: Result<()> = (|| {
                let mut file = private_options()
                    .write(true)
                    .create_new(true)
                    .open(&temp)
                    .map_err(|_| "Cannot create private image history")?;
                file.write_all(&bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|_| "Cannot save image history")?;
                drop(file);
                fs::rename(&temp, &path).map_err(|_| "Cannot commit image history")?;
                File::open(&self.root)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| "Cannot sync image history")?;
                Ok(())
            })();
            if saved.is_err() {
                let _ = fs::remove_file(temp);
            }
            saved?;
        }
        Ok(result)
    }
    fn recover(&self) -> Result<()> {
        self.locked(|doc|{let mut changed=false;for job in doc.jobs.values_mut(){if active(&job.status){job.status="interrupted".into();job.error=Some("The app stopped before this image request completed. It will not retry automatically.".into());job.updated_at=now();job.revision=job.revision.saturating_add(1);changed=true;}}Ok(((),changed))})
    }
    fn read(&self, id: &str) -> Result<Job> {
        self.locked(|doc| {
            Ok((
                doc.jobs.get(id).cloned().ok_or("Image job not found")?,
                false,
            ))
        })
    }
    fn update(&self, id: &str, f: impl FnOnce(&mut Job) -> Result<bool>) -> Result<Job> {
        self.locked(|doc| {
            let job = doc.jobs.get_mut(id).ok_or("Image job not found")?;
            let changed = f(job)?;
            if changed {
                job.updated_at = now();
                job.revision = job.revision.saturating_add(1);
            }
            Ok((job.clone(), changed))
        })
    }
}
fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn safe_file(path: &Path, missing: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => Ok(()),
        Err(e) if missing && e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err("Image history contains an unsafe file".into()),
    }
}
fn private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(path)
                .map_err(|_| "Cannot create private image storage")?;
        }
        _ => return Err("Image storage is not a private directory".into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "Cannot protect image storage")?;
    }
    Ok(())
}

impl Images {
    pub fn new(out: UnboundedSender<Value>) -> Self {
        Self::with_config(
            Options {
                root: crate::config::home().join("images"),
                base: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                timeout: Duration::from_secs(180),
            },
            Arc::new(|| {
                let snapshot = crate::auth::Store::configured()
                    .snapshot()
                    .map_err(|_| "Cannot read image credentials")?;
                Ok((snapshot.auth, snapshot.revision))
            }),
            Arc::new(crate::attachments::read_image_original),
            Arc::new(crate::attachments::import_generated_image),
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static Images HTTP client configuration"),
            out,
        )
    }
    fn with_config(
        options: Options,
        credentials: Credentials,
        read_image: ReadImage,
        save_image: SaveImage,
        http: reqwest::Client,
        out: UnboundedSender<Value>,
    ) -> Self {
        let store = Store {
            root: options.root.clone(),
        };
        let startup_error = store.recover().err();
        Self {
            inner: Arc::new(Inner {
                store,
                options,
                registry: Mutex::new(Registry::default()),
                startup_error,
                credentials,
                read_image,
                save_image,
                http,
                out,
            }),
        }
    }
    pub async fn handle(&self, method: &str, p: &Value) -> Option<Result<Value>> {
        if !matches!(
            method,
            "images/capabilities"
                | "images/generate"
                | "images/edit"
                | "images/job/read"
                | "images/job/list"
                | "images/job/cancel"
        ) {
            return None;
        }
        Some(match method {
            "images/capabilities" => Ok(self.capabilities()),
            "images/generate" | "images/edit" => self.enqueue(p, method == "images/edit").await,
            _ => {
                let inner = self.inner.clone();
                let method = method.to_owned();
                let p = p.clone();
                tokio::task::spawn_blocking(move || Self::rpc(&inner, &method, &p))
                    .await
                    .map_err(|_| "Image history worker stopped".into())
                    .and_then(|v| v)
            }
        })
    }
    fn capabilities(&self) -> Value {
        let account = (self.inner.credentials)();
        let closed = self
            .inner
            .registry
            .lock()
            .map(|registry| registry.closed)
            .unwrap_or(true);
        let available = !closed
            && self.inner.startup_error.is_none()
            && matches!(&account, Ok((Auth::ApiKey(_), _)));
        let reason=closed.then(||"The image service is closing".to_owned()).or_else(||self.inner.startup_error.clone()).or_else(||match account {Ok((Auth::ApiKey(_),_))=>None,Ok((Auth::ChatGpt{..},_))=>Some("The standard Images API requires an OpenAI API key. ChatGPT OAuth is not supported.".into()),_=>Some("Connect an OpenAI API key to generate or edit images.".into())});
        json!({"available":available,"reason":reason,"requiresApiKey":true,"chatgptOAuthSupported":false,"permissionsVerified":false,"defaultModel":DEFAULT_MODEL,
            "models":MODELS,"modelOptions":model_options(),"sizes":["auto","1024x1024","1536x1024","1024x1536"],"qualities":["auto","low","medium","high","xhigh","max"],"backgrounds":["auto","opaque","transparent"],"outputFormats":["png","jpeg","webp"],
            "limits":{"maxActiveJobs":2,"maxImages":4,"maxInputImages":8,"maxImageBytes":MAX_IMAGE,"maxInputTotalBytes":40*1024*1024,"maxInputPixels":32_000_000,"maxOutputPixels":8_294_400,"maxPromptCharacters":32000,"timeoutSeconds":self.inner.options.timeout.as_secs(),"maxSavedJobs":MAX_JOBS},
            "customSizes":{"models":["gpt-image-2.5-sunburst","gpt-image-2.5-flare","gpt-image-2.5-sunburst-2026-09-08","gpt-image-2.5-flare-2026-09-08","gpt-image-2","gpt-image-2-2026-04-21"],"step":16,"maxEdge":3840,"minPixels":655360,"maxPixels":8294400,"maxAspectRatio":3},
            "cancelStopsProvider":false,"cancelNote":CANCEL_NOTE,"automaticRetry":false})
    }
    async fn enqueue(&self, p: &Value, edit: bool) -> Result<Value> {
        if let Some(error) = &self.inner.startup_error {
            return Err(error.clone());
        }
        let job = parse_job(p, edit)?;
        let inner = self.inner.clone();
        let id = job.id.clone();
        let returned = job_value(&job);
        let generation = {
            let registry = inner
                .registry
                .lock()
                .map_err(|_| "Image controller lock is unavailable")?;
            if registry.closed {
                return Err("The image service is closing; no new requests can start".into());
            }
            registry.generation
        };
        let (auth, revision) = (inner.credentials)()?;
        let key=match auth {Auth::ApiKey(key) if !key.is_empty()=>key,Auth::ChatGpt{..}=>return Err("The standard Images API requires an OpenAI API key. ChatGPT OAuth is not supported.".into()),_=>return Err("Connect an OpenAI API key before starting an image request.".into())};
        endpoint(&inner.options.base, edit)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(Notify::new());
        {
            let mut registry = inner
                .registry
                .lock()
                .map_err(|_| "Image controller lock is unavailable")?;
            if registry.closed {
                return Err("The image service is closing; no new requests can start".into());
            }
            if registry.generation != generation {
                return Err(
                    "The account changed before this image request started. Start a new request."
                        .into(),
                );
            }
            if registry.running.len() >= 2 {
                return Err(
                    "Two image requests are already active. Wait for one to finish.".into(),
                );
            }
            inner.store.locked(|doc| {
                if doc.jobs.len() >= MAX_JOBS {
                    return Err("Image job history reached its local limit".into());
                }
                if doc.jobs.values().filter(|j| active(&j.status)).count() >= 2 {
                    return Err("Two image requests are already active".into());
                }
                doc.jobs.insert(id.clone(), job.clone());
                Ok(((), true))
            })?;
            registry.running.insert(
                id.clone(),
                Running {
                    cancelled: cancelled.clone(),
                    changed: changed.clone(),
                    generation,
                },
            );
            let worker_inner = inner.clone();
            let worker_id = id.clone();
            registry.workers.retain(|task| !task.is_finished());
            notify(&inner, &job);
            registry.workers.push(tokio::spawn(async move {
                run_job(
                    worker_inner,
                    worker_id,
                    key,
                    revision,
                    generation,
                    cancelled,
                    changed,
                )
                .await;
            }));
        }
        Ok(json!({"job":returned}))
    }
    fn rpc(inner: &Inner, method: &str, p: &Value) -> Result<Value> {
        if let Some(error) = &inner.startup_error {
            return Err(error.clone());
        }
        if method == "images/job/list" {
            let status = match p.get("status") {
                None | Some(Value::Null) => None,
                Some(Value::String(v)) if status_valid(v) => Some(v.as_str()),
                _ => return Err("Invalid image job status".into()),
            };
            let limit = match p.get("limit") {
                None | Some(Value::Null) => 100,
                Some(v) => v
                    .as_u64()
                    .filter(|v| (1..=1000).contains(v))
                    .ok_or("limit must be 1 to 1000")? as usize,
            };
            return inner.store.locked(|doc| {
                let mut jobs: Vec<_> = doc
                    .jobs
                    .values()
                    .filter(|j| status.is_none_or(|s| j.status == s))
                    .cloned()
                    .collect();
                jobs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.id.cmp(&b.id)));
                jobs.truncate(limit);
                Ok((json!({"data":jobs}), false))
            });
        }
        let id = p["id"]
            .as_str()
            .filter(|id| valid_job_id(id))
            .ok_or("A valid image job id is required")?;
        if method == "images/job/read" {
            return Ok(json!({"job":job_value(&inner.store.read(id)?)}));
        }
        let registry = inner
            .registry
            .lock()
            .map_err(|_| "Image controller lock is unavailable")?;
        let job = inner.store.update(id, |job| {
            if !active(&job.status) {
                return Ok(false);
            }
            job.status = "cancelled".into();
            job.cancel_note = Some(CANCEL_NOTE.into());
            job.error = None;
            Ok(true)
        })?;
        if let Some(running) = registry.running.get(id) {
            running.cancelled.store(true, Ordering::SeqCst);
            running.changed.notify_waiters();
        }
        drop(registry);
        notify(inner, &job);
        Ok(json!({"job":job_value(&job)}))
    }
    pub async fn shutdown(&self) {
        cancel_all(&self.inner);
        let workers = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            std::mem::take(&mut registry.workers)
        };
        for mut worker in workers {
            if tokio::time::timeout(Duration::from_secs(2), &mut worker)
                .await
                .is_err()
            {
                worker.abort();
            }
        }
    }
    /// Permanent EOF barrier; account changes use the reusable shutdown instead.
    pub async fn close(&self) {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed = true;
        self.shutdown().await;
    }
}
impl Drop for Images {
    fn drop(&mut self) {
        self.inner
            .registry
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .closed = true;
        cancel_all(&self.inner);
    }
}
fn cancel_all(inner: &Inner) {
    let mut registry = inner.registry.lock().unwrap_or_else(|p| p.into_inner());
    registry.generation = registry.generation.wrapping_add(1);
    for (id, running) in &registry.running {
        running.cancelled.store(true, Ordering::SeqCst);
        running.changed.notify_waiters();
        if let Ok(job)=inner.store.update(id,|job|{if !active(&job.status){return Ok(false);}job.status="interrupted".into();job.error=Some("The app or account changed before this request finished. It will not retry automatically.".into());job.cancel_note=Some(CANCEL_NOTE.into());Ok(true)}){notify(inner,&job);}
    }
}
fn parse_job(p: &Value, edit: bool) -> Result<Job> {
    if ["path", "imagePath", "maskPath"]
        .iter()
        .any(|name| p.get(name).is_some())
    {
        return Err("Image editing accepts attachment IDs, not local file paths".into());
    }
    let prompt = string(p, "prompt", "")?;
    if prompt.trim().is_empty() || prompt.chars().count() > 32000 {
        return Err("Image prompt must contain 1 to 32000 characters".into());
    }
    let model = string(p, "model", DEFAULT_MODEL)?;
    if !MODELS.contains(&model) {
        return Err("Choose a supported GPT Image model".into());
    }
    let quality = string(p, "quality", "auto")?;
    if !["auto", "low", "medium", "high", "xhigh", "max"].contains(&quality)
        || (["xhigh", "max"].contains(&quality) && !model.starts_with("gpt-image-2.5-"))
    {
        return Err("This image model does not support the selected quality".into());
    }
    let size = string(p, "size", "auto")?;
    if !["auto", "1024x1024", "1536x1024", "1024x1536"].contains(&size) {
        let (w, h) = size
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .ok_or("Image size must be WIDTHxHEIGHT or auto")?;
        let pixels = u64::from(w) * u64::from(h);
        if !(model.starts_with("gpt-image-2.5-")
            || model == "gpt-image-2"
            || model == "gpt-image-2-2026-04-21")
            || w == 0
            || h == 0
            || w % 16 != 0
            || h % 16 != 0
            || w > 3840
            || h > 3840
            || w > h.saturating_mul(3)
            || h > w.saturating_mul(3)
            || !(655360..=8294400).contains(&pixels)
        {
            return Err("Custom image size does not meet this model's resolution limits".into());
        }
    }
    let background = string(p, "background", "auto")?;
    if !["auto", "opaque", "transparent"].contains(&background) {
        return Err("Invalid image background".into());
    }
    let format = string(p, "outputFormat", "png")?;
    if !["png", "jpeg", "webp"].contains(&format) {
        return Err("Invalid image output format".into());
    }
    if background == "transparent" && format == "jpeg" {
        return Err("Transparent images require PNG or WebP output".into());
    }
    let n = match p.get("n") {
        None | Some(Value::Null) => 1,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=4).contains(n))
            .ok_or("n must be 1 to 4")? as u8,
    };
    let image_ids = if edit {
        let ids = p["imageIds"]
            .as_array()
            .filter(|ids| !ids.is_empty() && ids.len() <= 8)
            .ok_or("Editing requires 1 to 8 image attachment IDs")?;
        let mut result = Vec::new();
        for id in ids {
            let id = id
                .as_str()
                .filter(|id| valid_attachment(id))
                .ok_or("Invalid image attachment ID")?;
            if result.contains(&id.to_owned()) {
                return Err("Image attachment IDs must be unique".into());
            }
            result.push(id.to_owned());
        }
        result
    } else {
        if p.get("imageIds").is_some() || p.get("maskId").is_some() {
            return Err("Use images/edit for image references or masks".into());
        }
        Vec::new()
    };
    let mask_id = if edit {
        match p.get("maskId") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if valid_attachment(id) => Some(id.clone()),
            _ => return Err("Invalid mask attachment ID".into()),
        }
    } else {
        None
    };
    Ok(Job {
        id: format!("img_{:032x}", rand::random::<u128>()),
        kind: if edit { "edit" } else { "generate" }.into(),
        status: "queued".into(),
        prompt: prompt.into(),
        model: model.into(),
        size: size.into(),
        quality: quality.into(),
        background: background.into(),
        output_format: format.into(),
        n,
        image_ids,
        mask_id,
        created_at: now(),
        updated_at: now(),
        revision: 1,
        attachment_ids: Vec::new(),
        attachments: Vec::new(),
        error: None,
        cancel_note: None,
        usage: None,
    })
}
fn endpoint(base: &str, edit: bool) -> Result<String> {
    let url = reqwest::Url::parse(base).map_err(|_| "Invalid Images API base URL")?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Invalid Images API base URL".into());
    }
    Ok(format!(
        "{}/images/{}",
        base.trim_end_matches('/'),
        if edit { "edits" } else { "generations" }
    ))
}
fn check_image(bytes: Vec<u8>, maximum_pixels: u64) -> Result<Decoded> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE {
        return Err("An image exceeds the 10 MiB local limit".into());
    }
    let format = image::guess_format(&bytes).map_err(|_| "Provider returned an invalid image")?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::WebP => "image/webp",
        _ => return Err("Only PNG, JPEG and WebP images are supported".into()),
    };
    let (w, h) = image::ImageReader::with_format(Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| "Cannot read image dimensions")?;
    if w == 0 || h == 0 || w > 8192 || h > 8192 || u64::from(w) * u64::from(h) > maximum_pixels {
        return Err("Image dimensions exceed the local pixel limit".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "Image data could not be fully decoded")?;
    Ok(Decoded {
        bytes,
        mime,
        width: w,
        height: h,
        alpha: decoded.color().has_alpha(),
    })
}
async fn cancellation(cancelled: &AtomicBool, changed: &Notify) {
    loop {
        let notified = changed.notified();
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}
async fn run_job(
    inner: Arc<Inner>,
    id: String,
    key: String,
    revision: String,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    changed: Arc<Notify>,
) {
    let outcome:Result<()> = async {
        let started=inner.store.update(&id,|job|{if !active(&job.status){return Ok(false);}job.status="running".into();Ok(true)})?;
        if !active(&started.status)||cancelled.load(Ordering::SeqCst){return Ok(());}notify(&inner,&started);
        let request=request_images(&inner,&started,&key,&revision,generation,&cancelled);
        let (images,usage)=tokio::select!{biased;_=cancellation(&cancelled,&changed)=>return Ok(()),result=tokio::time::timeout(inner.options.timeout,request)=>result.map_err(|_|"Image provider request timed out. It was not retried.".to_owned())??};
        let committed_inner=inner.clone();let committed_id=id.clone();let expected_revision=revision.clone();
        tokio::task::spawn_blocking(move||{
            let registry=committed_inner.registry.lock().map_err(|_|"Image controller lock is unavailable")?;
            let permitted=registry.generation==generation&&registry.running.get(&committed_id).is_some_and(|run|run.generation==generation&&!run.cancelled.load(Ordering::SeqCst));
            if !permitted{return Ok(());}
            let current=(committed_inner.credentials)().map_err(|_|"Image credentials changed while the request was running")?;
            if current.1!=expected_revision||current.0!=Auth::ApiKey(key){
                let job=committed_inner.store.update(&committed_id,|job|{if !active(&job.status){return Ok(false);}job.status="interrupted".into();job.error=Some("Credentials changed before image results were saved. The request was not retried.".into());Ok(true)})?;notify(&committed_inner,&job);return Ok(());
            }
            let job=committed_inner.store.update(&committed_id,|job|{
                if !active(&job.status){return Ok(false);}
                for (index,image) in images.iter().enumerate(){
                    let extension=image.mime.strip_prefix("image/").unwrap();let name=format!("Generated image {}-{}.{}",&job.id[4..12],index+1,extension);
                    match (committed_inner.save_image)(&name,image.mime,&image.bytes){
                        Ok(metadata)=>{let attachment_id=metadata["id"].as_str().filter(|id|valid_attachment(id)).ok_or("Generated attachment returned an invalid ID")?;job.attachment_ids.push(attachment_id.to_owned());job.attachments.push(metadata);},
                        Err(_)=>{job.status="failed".into();job.error=Some("Could not save generated images. Any saved results remain available in Library.".into());return Ok(true);}
                    }
                }
                job.usage=usage;job.status="completed".into();job.error=None;Ok(true)
            })?;drop(registry);notify(&committed_inner,&job);Ok(())
        }).await.map_err(|_|"Image save worker stopped")?
    }.await;
    if let Err(error) = outcome {
        let registry = inner.registry.lock().unwrap_or_else(|p| p.into_inner());
        if registry.generation == generation && !cancelled.load(Ordering::SeqCst) {
            if let Ok(job) = inner.store.update(&id, |job| {
                if !active(&job.status) {
                    return Ok(false);
                }
                job.status = if error == CREDENTIALS_CHANGED {
                    "interrupted"
                } else {
                    "failed"
                }
                .into();
                job.error = Some(error);
                Ok(true)
            }) {
                notify(&inner, &job);
            }
        }
    }
    inner
        .registry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .running
        .remove(&id);
}
fn verify_request_identity(
    inner: &Inner,
    key: &str,
    revision: &str,
    generation: u64,
    cancelled: &AtomicBool,
) -> Result<()> {
    let permitted = |registry: &Registry| {
        !registry.closed && registry.generation == generation && !cancelled.load(Ordering::SeqCst)
    };
    if !permitted(
        &*inner
            .registry
            .lock()
            .map_err(|_| "Image controller lock is unavailable")?,
    ) {
        return Err(CREDENTIALS_CHANGED.into());
    }
    let (auth, current_revision) = (inner.credentials)().map_err(|_| CREDENTIALS_CHANGED)?;
    if current_revision != revision || !matches!(auth,Auth::ApiKey(ref current) if current==key) {
        return Err(CREDENTIALS_CHANGED.into());
    }
    if !permitted(
        &*inner
            .registry
            .lock()
            .map_err(|_| "Image controller lock is unavailable")?,
    ) {
        return Err(CREDENTIALS_CHANGED.into());
    }
    Ok(())
}
async fn request_images(
    inner: &Inner,
    job: &Job,
    key: &str,
    revision: &str,
    generation: u64,
    cancelled: &AtomicBool,
) -> Result<(Vec<Decoded>, Option<Value>)> {
    verify_request_identity(inner, key, revision, generation, cancelled)?;
    let url = endpoint(&inner.options.base, job.kind == "edit")?;
    let request = if job.kind == "edit" {
        let ids = job.image_ids.clone();
        let mask_id = job.mask_id.clone();
        let read = inner.read_image.clone();
        let (sources,mask)=tokio::task::spawn_blocking(move||{
            let mut sources=Vec::new();let mut total=0;
            for id in ids{let (metadata,bytes)=read(&id).map_err(|_|"Cannot read a source image attachment")?;if metadata["kind"]!="image"||metadata["parseStatus"]!="ready"{return Err("Source attachment is not a readable image".to_string());}total+=bytes.len();if total>40*1024*1024{return Err("Edit inputs exceed the 40 MiB local limit".into());}sources.push(check_image(bytes,32_000_000)?);}
            let mask=if let Some(id)=mask_id{let (_,bytes)=read(&id).map_err(|_|"Cannot read the mask image attachment")?;if bytes.len()>=4*1024*1024{return Err("Mask must be smaller than 4 MiB".into());}let mask=check_image(bytes,32_000_000)?;if mask.mime!="image/png"||!mask.alpha||mask.width!=sources[0].width||mask.height!=sources[0].height{return Err("Mask must be a PNG with an alpha channel and match the first image dimensions".into());}Some(mask)}else{None};Ok((sources,mask))
        }).await.map_err(|_|"Image input worker stopped")??;
        let mut form = Form::new()
            .text("model", job.model.clone())
            .text("prompt", job.prompt.clone())
            .text("size", job.size.clone())
            .text("quality", job.quality.clone())
            .text("background", job.background.clone())
            .text("output_format", job.output_format.clone())
            .text("n", job.n.to_string());
        for (index, source) in sources.into_iter().enumerate() {
            let filename = format!(
                "input-{}.{}",
                index + 1,
                source.mime.strip_prefix("image/").unwrap()
            );
            form = form.part(
                "image[]",
                Part::bytes(source.bytes)
                    .file_name(filename)
                    .mime_str(source.mime)
                    .map_err(|_| "Invalid source image MIME")?,
            );
        }
        if let Some(mask) = mask {
            form = form.part(
                "mask",
                Part::bytes(mask.bytes)
                    .file_name("mask.png")
                    .mime_str("image/png")
                    .unwrap(),
            );
        }
        inner.http.post(url).bearer_auth(key).multipart(form)
    } else {
        inner.http.post(url).bearer_auth(key).json(&json!({"model":job.model,"prompt":job.prompt,"size":job.size,"quality":job.quality,"background":job.background,"output_format":job.output_format,"n":job.n}))
    };
    verify_request_identity(inner, key, revision, generation, cancelled)?;
    let mut response = request
        .send()
        .await
        .map_err(|_| "Could not reach the image provider. The request was not retried.")?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 => "The image provider rejected this API key.".into(),
            403 => "This API key does not have permission to use the selected image model.".into(),
            429 => {
                "Image provider usage or rate limit reached. The request was not retried.".into()
            }
            status => format!(
                "Image provider rejected the request (HTTP {status}). The request was not retried."
            ),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE as u64)
    {
        return Err("Image provider response exceeds the local limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Image provider response stopped unexpectedly")?
    {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err("Image provider response exceeds the local limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let document: Value =
        serde_json::from_slice(&bytes).map_err(|_| "Image provider returned invalid JSON")?;
    let data = document["data"]
        .as_array()
        .filter(|data| data.len() == usize::from(job.n))
        .ok_or("Image provider returned an unexpected number of images")?;
    let mut decoded = Vec::new();
    for item in data {
        let encoded = item["b64_json"].as_str().ok_or(
            "Image provider did not return base64 image data; remote image URLs are not fetched",
        )?;
        if encoded.len() > MAX_IMAGE.div_ceil(3) * 4 {
            return Err("Generated image exceeds the 10 MiB local limit".into());
        }
        let image = check_image(
            STANDARD
                .decode(encoded)
                .map_err(|_| "Image provider returned invalid base64 data")?,
            8_294_400,
        )?;
        if image.mime != format!("image/{}", job.output_format) {
            return Err("Generated image format does not match the requested format".into());
        }
        if job.background == "transparent" && !image.alpha {
            return Err("Generated transparent image is missing an alpha channel".into());
        }
        decoded.push(image);
    }
    let usage = document.get("usage").and_then(|usage| {
        let mut public = serde_json::Map::new();
        for name in ["input_tokens", "output_tokens", "total_tokens"] {
            if let Some(value) = usage[name].as_u64() {
                public.insert(name.into(), json!(value));
            }
        }
        (!public.is_empty()).then_some(Value::Object(public))
    });
    Ok((decoded, usage))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{mpsc, oneshot},
    };

    struct Reply {
        status: u16,
        body: Vec<u8>,
        gate: Option<oneshot::Receiver<()>>,
        declared: Option<usize>,
    }
    impl Reply {
        fn json(value: Value) -> Self {
            Self {
                status: 200,
                body: serde_json::to_vec(&value).unwrap(),
                gate: None,
                declared: None,
            }
        }
    }
    struct Mock {
        base: String,
        requests: mpsc::UnboundedReceiver<Vec<u8>>,
        task: JoinHandle<()>,
    }
    impl Mock {
        async fn new(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            let (tx, requests) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                for reply in replies {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let mut bytes = Vec::new();
                        let mut buffer = [0; 8192];
                        loop {
                            let count = socket.read(&mut buffer).await.unwrap_or(0);
                            if count == 0 {
                                break;
                            }
                            bytes.extend_from_slice(&buffer[..count]);
                            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                                let headers = String::from_utf8_lossy(&bytes[..end]);
                                let length = headers
                                    .lines()
                                    .filter_map(|line| line.split_once(':'))
                                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                                    .unwrap_or(0);
                                if bytes.len() >= end + 4 + length {
                                    break;
                                }
                            }
                            if bytes.len() > MAX_RESPONSE {
                                break;
                            }
                        }
                        let _ = tx.send(bytes);
                        if let Some(gate) = reply.gate {
                            let _ = tokio::time::timeout(Duration::from_secs(3), gate).await;
                        }
                        let headers=format!("HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.status,reply.declared.unwrap_or(reply.body.len()));
                        let _ = socket.write_all(headers.as_bytes()).await;
                        let _ = socket.write_all(&reply.body).await;
                    });
                }
            });
            Self {
                base,
                requests,
                task,
            }
        }
        async fn request(&mut self) -> Vec<u8> {
            tokio::time::timeout(Duration::from_secs(3), self.requests.recv())
                .await
                .unwrap()
                .unwrap()
        }
    }
    impl Drop for Mock {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    type Assets = Arc<Mutex<BTreeMap<String, (Value, Vec<u8>)>>>;
    struct Fixture {
        dir: PathBuf,
        images: Images,
        events: mpsc::UnboundedReceiver<Value>,
        credentials: Arc<Mutex<(Auth, String)>>,
        assets: Assets,
    }
    fn png(alpha: bool, width: u32, height: u32) -> Vec<u8> {
        let image = if alpha {
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(
                width,
                height,
                Rgba([20, 40, 60, 128]),
            ))
        } else {
            DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, Rgb([20, 40, 60])))
        };
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, ImageFormat::Png).unwrap();
        cursor.into_inner()
    }
    fn success(bytes: &[u8]) -> Reply {
        Reply::json(
            json!({"data":[{"b64_json":STANDARD.encode(bytes)}],"usage":{"input_tokens":13,"output_tokens":42,"total_tokens":55,"secret":"must-not-be-public"}}),
        )
    }
    impl Fixture {
        fn new(base: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("mcodex-images-{:032x}", rand::random::<u128>()));
            fs::create_dir_all(&dir).unwrap();
            let credentials = Arc::new(Mutex::new((
                Auth::ApiKey("fixture-images-secret".into()),
                "revision-A".into(),
            )));
            let assets = Arc::new(Mutex::new(BTreeMap::new()));
            let (images, events) = Self::controller(
                &dir,
                base,
                credentials.clone(),
                assets.clone(),
                Duration::from_secs(2),
            );
            Self {
                dir,
                images,
                events,
                credentials,
                assets,
            }
        }
        fn controller(
            dir: &Path,
            base: &str,
            credentials: Arc<Mutex<(Auth, String)>>,
            assets: Assets,
            timeout: Duration,
        ) -> (Images, mpsc::UnboundedReceiver<Value>) {
            let (out, events) = mpsc::unbounded_channel();
            let reads = assets.clone();
            let saves = assets;
            let output = dir.join("assets");
            fs::create_dir_all(&output).unwrap();
            let images = Images::with_config(
                Options {
                    root: dir.join("jobs"),
                    base: base.into(),
                    timeout,
                },
                Arc::new(move || Ok(credentials.lock().unwrap().clone())),
                Arc::new(move |id| {
                    reads
                        .lock()
                        .unwrap()
                        .get(id)
                        .cloned()
                        .ok_or("Fixture image not found".into())
                }),
                Arc::new(move |name, mime, bytes| {
                    let id = format!("att_{:032x}", rand::random::<u128>());
                    let decoded = check_image(bytes.to_vec(), 32_000_000)?;
                    let temp = output.join(format!(".{id}.tmp"));
                    let path = output.join(&id);
                    let mut file = private_options()
                        .write(true)
                        .create_new(true)
                        .open(&temp)
                        .unwrap();
                    file.write_all(bytes).unwrap();
                    file.sync_all().unwrap();
                    drop(file);
                    fs::rename(temp, path).unwrap();
                    let metadata = json!({"id":id,"name":name,"mimeType":mime,"kind":"image","parseStatus":"ready","size":bytes.len(),"width":decoded.width,"height":decoded.height});
                    saves
                        .lock()
                        .unwrap()
                        .insert(id, (metadata.clone(), bytes.to_vec()));
                    Ok(metadata)
                }),
                reqwest::Client::builder()
                    .no_proxy()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .unwrap(),
                out,
            );
            (images, events)
        }
        fn source(&self, bytes: Vec<u8>) -> String {
            let id = format!("att_{:032x}", rand::random::<u128>());
            self.assets.lock().unwrap().insert(
                id.clone(),
                (json!({"id":id,"kind":"image","parseStatus":"ready"}), bytes),
            );
            id
        }
        async fn rpc(&self, method: &str, p: Value) -> Result<Value> {
            self.images
                .handle(method, &p)
                .await
                .expect("handled Images RPC")
        }
        async fn generate(&self) -> Value {
            self.rpc("images/generate", json!({"prompt":"A fixture image"}))
                .await
                .unwrap()["job"]
                .clone()
        }
        async fn terminal(&self, id: &str) -> Value {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let job =
                        self.rpc("images/job/read", json!({"id":id})).await.unwrap()["job"].clone();
                    if !active(job["status"].as_str().unwrap()) {
                        return job;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            cancel_all(&self.images.inner);
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn model_contract_enforces_current_and_legacy_differences() {
        let default = parse_job(&json!({"prompt":"Draw"}), false).unwrap();
        assert_eq!(default.model, DEFAULT_MODEL);
        assert_eq!(default.n, 1);
        for model in [
            "gpt-image-2.5-sunburst",
            "gpt-image-2.5-flare",
            "gpt-image-2.5-sunburst-2026-09-08",
        ] {
            assert!(parse_job(&json!({"prompt":"Draw","model":model,"quality":"max","size":"1536x864","background":"transparent","outputFormat":"webp"}),false).is_ok());
        }
        assert!(parse_job(
            &json!({"prompt":"Draw","model":"gpt-image-2","size":"3840x2160","quality":"high"}),
            false
        )
        .is_ok());
        for p in [
            json!({"prompt":"Draw","model":"gpt-image-2","quality":"xhigh"}),
            json!({"prompt":"Draw","model":"gpt-image-1.5","size":"1536x864"}),
            json!({"prompt":"Draw","size":"3840x3840"}),
            json!({"prompt":"Draw","size":"1000x1000"}),
            json!({"prompt":"Draw","background":"transparent","outputFormat":"jpeg"}),
            json!({"prompt":"Draw","n":0}),
            json!({"prompt":"Draw","n":5}),
            json!({"prompt":"Draw","model":"dall-e-3"}),
        ] {
            assert!(parse_job(&p, false).is_err(), "{p}");
        }
        assert!(parse_job(&json!({"prompt":"x".repeat(32001)}), false).is_err());
        assert!(parse_job(&json!({"prompt":"Edit","imageIds":["/etc/passwd"]}), true).is_err());
    }

    #[tokio::test]
    async fn generation_returns_before_http_and_saves_real_image_bytes_and_usage() {
        let bytes = png(true, 2, 2);
        let (release, gate) = oneshot::channel();
        let mut reply = success(&bytes);
        reply.gate = Some(gate);
        let mut mock = Mock::new(vec![reply]).await;
        let mut f = Fixture::new(&mock.base);
        let queued = f.generate().await;
        assert_eq!(queued["status"], "queued");
        assert_eq!(queued["attachmentIds"], json!([]));
        let raw = mock.request().await;
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("POST /v1/images/generations HTTP/1.1"));
        assert!(text
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-images-secret"));
        let end = raw.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
        let request: Value = serde_json::from_slice(&raw[end + 4..]).unwrap();
        assert_eq!(request["model"], DEFAULT_MODEL);
        assert_eq!(request["output_format"], "png");
        assert!(request.get("response_format").is_none());
        assert!(f.assets.lock().unwrap().is_empty());
        release.send(()).unwrap();
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "completed");
        assert_eq!(
            job["usage"],
            json!({"input_tokens":13,"output_tokens":42,"total_tokens":55})
        );
        let id = job["attachmentIds"][0].as_str().unwrap();
        assert_eq!(fs::read(f.dir.join("assets").join(id)).unwrap(), bytes);
        assert_eq!(job["attachments"][0]["width"], 2);
        let mut completed = false;
        while let Ok(event) = f.events.try_recv() {
            assert!(!event.to_string().contains("fixture-images-secret"));
            if event["params"]["job"]["status"] == "completed" {
                completed = true;
                assert_eq!(event["params"]["job"]["attachmentIds"][0], id);
            }
        }
        assert!(completed);
        assert!(!fs::read_to_string(f.dir.join("jobs/jobs.json"))
            .unwrap()
            .contains("fixture-images-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(f.dir.join("jobs/jobs.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(f.dir.join("jobs"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn edit_sends_original_bytes_and_mask_as_multipart_and_generated_results_can_be_edited() {
        let bytes = png(true, 2, 2);
        let mut mock = Mock::new(vec![success(&bytes), success(&bytes)]).await;
        let f = Fixture::new(&mock.base);
        let image = f.source(bytes.clone());
        let mask = f.source(png(true, 2, 2));
        let queued=f.rpc("images/edit",json!({"prompt":"Change the blue area","imageIds":[image],"maskId":mask,"quality":"xhigh"})).await.unwrap()["job"].clone();
        let raw = mock.request().await;
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("POST /v1/images/edits HTTP/1.1"));
        assert!(text.contains("multipart/form-data; boundary="));
        assert!(text.contains("name=\"image[]\""));
        assert!(text.contains("filename=\"input-1.png\""));
        assert!(text.contains("name=\"mask\""));
        assert!(text.contains("name=\"quality\"\r\n\r\nxhigh"));
        assert!(raw.windows(bytes.len()).any(|window| window == bytes));
        assert!(!text.contains(f.dir.to_str().unwrap()));
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "completed");
        let second = f
            .rpc(
                "images/edit",
                json!({"prompt":"A second edit","imageIds":job["attachmentIds"]}),
            )
            .await
            .unwrap()["job"]
            .clone();
        mock.request().await;
        assert_eq!(
            f.terminal(second["id"].as_str().unwrap()).await["status"],
            "completed"
        );
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn invalid_masks_and_corrupt_inputs_fail_before_a_provider_request() {
        let f = Fixture::new("http://127.0.0.1:9/v1");
        let source = f.source(png(true, 2, 2));
        for mask in [f.source(png(false, 2, 2)), f.source(png(true, 3, 3))] {
            let queued = f
                .rpc(
                    "images/edit",
                    json!({"prompt":"Edit","imageIds":[source],"maskId":mask}),
                )
                .await
                .unwrap()["job"]
                .clone();
            let job = f.terminal(queued["id"].as_str().unwrap()).await;
            assert_eq!(job["status"], "failed");
            assert!(job["error"].as_str().unwrap().contains("alpha channel"));
        }
        let bad = f.source(b"not an image".to_vec());
        let queued = f
            .rpc("images/edit", json!({"prompt":"Edit","imageIds":[bad]}))
            .await
            .unwrap()["job"]
            .clone();
        assert_eq!(
            f.terminal(queued["id"].as_str().unwrap()).await["status"],
            "failed"
        );
        assert_eq!(f.assets.lock().unwrap().len(), 4);
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn cancellation_is_durable_idempotent_and_does_not_save_a_late_success() {
        let bytes = png(true, 2, 2);
        let (release, gate) = oneshot::channel();
        let mut reply = success(&bytes);
        reply.gate = Some(gate);
        let mut mock = Mock::new(vec![reply]).await;
        let f = Fixture::new(&mock.base);
        let queued = f.generate().await;
        mock.request().await;
        let id = queued["id"].as_str().unwrap();
        let cancelled = f.rpc("images/job/cancel", json!({"id":id})).await.unwrap();
        assert_eq!(cancelled["job"]["status"], "cancelled");
        assert!(cancelled["job"]["cancelNote"]
            .as_str()
            .unwrap()
            .contains("may still finish"));
        assert_eq!(
            f.rpc("images/job/cancel", json!({"id":id})).await.unwrap()["job"]["status"],
            "cancelled"
        );
        let _ = release.send(());
        f.images.shutdown().await;
        assert!(f.assets.lock().unwrap().is_empty());
        assert_eq!(f.terminal(id).await["status"], "cancelled");
        let (reopened, _) = Fixture::controller(
            &f.dir,
            &mock.base,
            f.credentials.clone(),
            f.assets.clone(),
            Duration::from_secs(2),
        );
        assert_eq!(
            reopened
                .handle("images/job/read", &json!({"id":id}))
                .await
                .unwrap()
                .unwrap()["job"]["status"],
            "cancelled"
        );
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn credentials_revision_protects_against_late_results_without_shutdown() {
        let bytes = png(true, 2, 2);
        let (release, gate) = oneshot::channel();
        let mut reply = success(&bytes);
        reply.gate = Some(gate);
        let mut mock = Mock::new(vec![reply]).await;
        let f = Fixture::new(&mock.base);
        let queued = f.generate().await;
        mock.request().await;
        *f.credentials.lock().unwrap() =
            (Auth::ApiKey("new-fixture-key".into()), "revision-B".into());
        release.send(()).unwrap();
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "interrupted");
        assert!(f.assets.lock().unwrap().is_empty());
        assert!(!job.to_string().contains("new-fixture-key"));
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_interrupts_old_generation_but_allows_work_after_a_new_login() {
        let bytes = png(true, 2, 2);
        let (release, gate) = oneshot::channel();
        let mut reply = success(&bytes);
        reply.gate = Some(gate);
        let mut mock = Mock::new(vec![reply, success(&bytes)]).await;
        let f = Fixture::new(&mock.base);
        let queued = f.generate().await;
        mock.request().await;
        f.images.shutdown().await;
        let _ = release.send(());
        assert_eq!(
            f.terminal(queued["id"].as_str().unwrap()).await["status"],
            "interrupted"
        );
        assert!(f.assets.lock().unwrap().is_empty());
        let second = f.generate().await;
        mock.request().await;
        assert_eq!(
            f.terminal(second["id"].as_str().unwrap()).await["status"],
            "completed"
        );
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn restart_keeps_completed_images_and_marks_inflight_interrupted_without_retry() {
        let bytes = png(true, 2, 2);
        let mut mock = Mock::new(vec![success(&bytes)]).await;
        let f = Fixture::new(&mock.base);
        let queued = f.generate().await;
        mock.request().await;
        let completed = f.terminal(queued["id"].as_str().unwrap()).await;
        f.images.shutdown().await;
        let inflight = parse_job(&json!({"prompt":"Unfinished request"}), false).unwrap();
        f.images
            .inner
            .store
            .locked(|doc| {
                doc.jobs.insert(inflight.id.clone(), inflight.clone());
                Ok(((), true))
            })
            .unwrap();
        let (reopened, _) = Fixture::controller(
            &f.dir,
            &mock.base,
            f.credentials.clone(),
            f.assets.clone(),
            Duration::from_secs(2),
        );
        let complete = reopened
            .handle("images/job/read", &json!({"id":completed["id"]}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(complete["job"], completed);
        assert_eq!(
            fs::read(
                f.dir
                    .join("assets")
                    .join(completed["attachmentIds"][0].as_str().unwrap())
            )
            .unwrap(),
            bytes
        );
        let interrupted = reopened
            .handle("images/job/read", &json!({"id":inflight.id}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(interrupted["job"]["status"], "interrupted");
        assert!(interrupted["job"]["error"]
            .as_str()
            .unwrap()
            .contains("will not retry"));
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn oauth_and_signed_out_accounts_cannot_generate_images() {
        let f = Fixture::new("http://127.0.0.1:9/v1");
        for auth in [
            Auth::Unavailable,
            Auth::ChatGpt {
                access: "private-oauth-access".into(),
                refresh: "private-refresh".into(),
                account_id: "private-id".into(),
            },
        ] {
            *f.credentials.lock().unwrap() = (auth, "different".into());
            let cap = f.rpc("images/capabilities", json!({})).await.unwrap();
            assert_eq!(cap["available"], false);
            assert_eq!(cap["chatgptOAuthSupported"], false);
            assert!(f
                .rpc("images/generate", json!({"prompt":"Draw"}))
                .await
                .is_err());
            assert!(!cap.to_string().contains("private-"));
        }
        assert!(f.rpc("images/job/list", json!({})).await.unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty());
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn provider_errors_bad_base64_invalid_images_and_urls_never_count_as_success() {
        let replies = vec![
            Reply {
                status: 401,
                body: b"fixture-images-secret diagnostic".to_vec(),
                gate: None,
                declared: None,
            },
            Reply {
                status: 429,
                body: b"fixture-images-secret".to_vec(),
                gate: None,
                declared: None,
            },
            Reply::json(json!({"data":[{"url":"http://127.0.0.1/private"}]})),
            Reply::json(json!({"data":[{"b64_json":"not base64 fixture-images-secret"}]})),
            Reply::json(json!({"data":[{"b64_json":STANDARD.encode(b"fake png")}]})),
            Reply::json(json!({"data":[]})),
            Reply {
                status: 200,
                body: b"not json fixture-images-secret".to_vec(),
                gate: None,
                declared: None,
            },
            Reply {
                status: 200,
                body: Vec::new(),
                gate: None,
                declared: Some(MAX_RESPONSE + 1),
            },
        ];
        for reply in replies {
            let mut mock = Mock::new(vec![reply]).await;
            let f = Fixture::new(&mock.base);
            let queued = f.generate().await;
            mock.request().await;
            let job = f.terminal(queued["id"].as_str().unwrap()).await;
            assert_eq!(job["status"], "failed");
            assert!(!job["error"]
                .as_str()
                .unwrap()
                .contains("fixture-images-secret"));
            assert_eq!(job["attachmentIds"], json!([]));
            assert!(f.assets.lock().unwrap().is_empty());
            f.images.shutdown().await;
        }
    }

    #[tokio::test]
    async fn timeout_is_not_retried_and_cannot_save_late_images() {
        let bytes = png(true, 2, 2);
        let (release, gate) = oneshot::channel();
        let mut reply = success(&bytes);
        reply.gate = Some(gate);
        let mut mock = Mock::new(vec![reply]).await;
        let f = Fixture::new(&mock.base);
        let (images, _) = Fixture::controller(
            &f.dir,
            &mock.base,
            f.credentials.clone(),
            f.assets.clone(),
            Duration::from_millis(30),
        );
        let queued = images
            .handle("images/generate", &json!({"prompt":"Timeout fixture"}))
            .await
            .unwrap()
            .unwrap()["job"]
            .clone();
        mock.request().await;
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "failed");
        assert!(job["error"].as_str().unwrap().contains("timed out"));
        let _ = release.send(());
        images.shutdown().await;
        assert!(f.assets.lock().unwrap().is_empty());
        f.images.shutdown().await;
    }

    #[test]
    fn full_image_decoder_rejects_truncated_data_and_pixel_bombs() {
        let bytes = png(true, 2, 2);
        assert!(check_image(bytes[..33].to_vec(), 32_000_000).is_err());
        assert!(check_image(bytes.clone(), 3).is_err());
        assert!(check_image(vec![0; MAX_IMAGE + 1], 32_000_000).is_err());
        assert!(check_image(bytes, 32_000_000).unwrap().alpha);
    }

    #[tokio::test]
    async fn damaged_history_is_preserved_and_cannot_be_overwritten() {
        let f = Fixture::new("http://127.0.0.1:9/v1");
        let path = f.dir.join("jobs/jobs.json");
        fs::write(&path, b"damaged fixture history").unwrap();
        let (images, _) = Fixture::controller(
            &f.dir,
            "http://127.0.0.1:9/v1",
            f.credentials.clone(),
            f.assets.clone(),
            Duration::from_secs(2),
        );
        assert_eq!(
            images
                .handle("images/capabilities", &json!({}))
                .await
                .unwrap()
                .unwrap()["available"],
            false
        );
        assert!(images
            .handle("images/generate", &json!({"prompt":"Draw"}))
            .await
            .unwrap()
            .is_err());
        assert!(images
            .handle("images/job/list", &json!({}))
            .await
            .unwrap()
            .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"damaged fixture history");
        images.shutdown().await;
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn active_limit_is_enforced_and_completed_jobs_cannot_be_cancelled() {
        let bytes = png(true, 2, 2);
        let (release_a, gate_a) = oneshot::channel();
        let (release_b, gate_b) = oneshot::channel();
        let mut a = success(&bytes);
        a.gate = Some(gate_a);
        let mut b = success(&bytes);
        b.gate = Some(gate_b);
        let mut mock = Mock::new(vec![a, b]).await;
        let f = Fixture::new(&mock.base);
        let first = f.generate().await;
        mock.request().await;
        let second = f.generate().await;
        mock.request().await;
        assert!(f
            .rpc("images/generate", json!({"prompt":"Third job"}))
            .await
            .unwrap_err()
            .contains("already active"));
        release_a.send(()).unwrap();
        release_b.send(()).unwrap();
        let complete = f.terminal(first["id"].as_str().unwrap()).await;
        assert_eq!(complete["status"], "completed");
        assert!(complete["revision"].as_u64().unwrap() > first["revision"].as_u64().unwrap());
        assert_eq!(
            f.terminal(second["id"].as_str().unwrap()).await["status"],
            "completed"
        );
        assert_eq!(
            f.rpc("images/job/cancel", json!({"id":first["id"]}))
                .await
                .unwrap()["job"]["status"],
            "completed"
        );
        let listed = f
            .rpc("images/job/list", json!({"status":"completed","limit":1}))
            .await
            .unwrap();
        assert_eq!(listed["data"].as_array().unwrap().len(), 1);
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn a_partial_storage_failure_keeps_saved_artifacts_but_does_not_claim_completion() {
        let bytes = png(true, 2, 2);
        let mut mock=Mock::new(vec![Reply::json(json!({"data":[{"b64_json":STANDARD.encode(&bytes)},{"b64_json":STANDARD.encode(&bytes)}]}))]).await;
        let f = Fixture::new(&mock.base);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let original = f.images.inner.save_image.clone();
        let saves = count.clone();
        let (out, _) = mpsc::unbounded_channel();
        let images = Images::with_config(
            f.images.inner.options.clone(),
            f.images.inner.credentials.clone(),
            f.images.inner.read_image.clone(),
            Arc::new(move |name, mime, bytes| {
                if saves.fetch_add(1, Ordering::SeqCst) == 1 {
                    return Err("fixture-images-secret disk detail".into());
                }
                original(name, mime, bytes)
            }),
            f.images.inner.http.clone(),
            out,
        );
        let queued = images
            .handle("images/generate", &json!({"prompt":"Two images","n":2}))
            .await
            .unwrap()
            .unwrap()["job"]
            .clone();
        mock.request().await;
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "failed");
        assert_eq!(job["attachmentIds"].as_array().unwrap().len(), 1);
        let saved = job["attachmentIds"][0].as_str().unwrap();
        assert_eq!(fs::read(f.dir.join("assets").join(saved)).unwrap(), bytes);
        assert!(!job["error"]
            .as_str()
            .unwrap()
            .contains("fixture-images-secret"));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        images.shutdown().await;
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn transparent_and_requested_formats_are_checked_against_decoded_bytes() {
        for p in [
            json!({"prompt":"Draw","background":"transparent"}),
            json!({"prompt":"Draw","outputFormat":"jpeg"}),
        ] {
            let bytes = png(false, 2, 2);
            let mut mock = Mock::new(vec![success(&bytes)]).await;
            let f = Fixture::new(&mock.base);
            let queued = f.rpc("images/generate", p).await.unwrap()["job"].clone();
            mock.request().await;
            let job = f.terminal(queued["id"].as_str().unwrap()).await;
            assert_eq!(job["status"], "failed");
            assert!(f.assets.lock().unwrap().is_empty());
            f.images.shutdown().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_blocks_a_delayed_create_that_loaded_credentials_before_eof() {
        let f = Fixture::new("http://127.0.0.1:9/v1");
        let (entered, mut received) = mpsc::unbounded_channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let waiting = barrier.clone();
        let (out, _) = mpsc::unbounded_channel();
        let images = Arc::new(Images::with_config(
            f.images.inner.options.clone(),
            Arc::new(move || {
                let _ = entered.send(());
                waiting.wait();
                Ok((
                    Auth::ApiKey("fixture-images-secret".into()),
                    "revision-A".into(),
                ))
            }),
            f.images.inner.read_image.clone(),
            f.images.inner.save_image.clone(),
            f.images.inner.http.clone(),
            out,
        ));
        let delayed = images.clone();
        let create = tokio::spawn(async move {
            delayed
                .handle("images/generate", &json!({"prompt":"Racing EOF"}))
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        images.close().await;
        barrier.wait();
        let error = tokio::time::timeout(Duration::from_secs(2), create)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("closing"));
        assert!(images
            .inner
            .store
            .locked(|doc| Ok((doc.jobs.is_empty(), false)))
            .unwrap());
        assert!(f.assets.lock().unwrap().is_empty());
        images.shutdown().await;
        assert!(images.inner.registry.lock().unwrap().closed);
        f.images.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn account_shutdown_rejects_a_create_that_already_loaded_old_credentials() {
        let bytes = png(true, 2, 2);
        let mut mock = Mock::new(vec![success(&bytes)]).await;
        let f = Fixture::new(&mock.base);
        let (entered, mut received) = mpsc::unbounded_channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let waiting = barrier.clone();
        let first = AtomicBool::new(true);
        let credentials = f.credentials.clone();
        let (out, _) = mpsc::unbounded_channel();
        let images = Arc::new(Images::with_config(
            f.images.inner.options.clone(),
            Arc::new(move || {
                let snapshot = credentials.lock().unwrap().clone();
                if first.swap(false, Ordering::SeqCst) {
                    let _ = entered.send(());
                    waiting.wait();
                }
                Ok(snapshot)
            }),
            f.images.inner.read_image.clone(),
            f.images.inner.save_image.clone(),
            f.images.inner.http.clone(),
            out,
        ));
        let delayed = images.clone();
        let create = tokio::spawn(async move {
            delayed
                .handle("images/generate", &json!({"prompt":"Old account race"}))
                .await
                .unwrap()
        });
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        *f.credentials.lock().unwrap() =
            (Auth::ApiKey("new-fixture-key".into()), "revision-B".into());
        images.shutdown().await;
        barrier.wait();
        let error = tokio::time::timeout(Duration::from_secs(2), create)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("account changed"));
        assert!(images
            .inner
            .store
            .locked(|doc| Ok((doc.jobs.is_empty(), false)))
            .unwrap());
        assert!(mock.requests.try_recv().is_err());
        let queued = images
            .handle(
                "images/generate",
                &json!({"prompt":"New account remains usable"}),
            )
            .await
            .unwrap()
            .unwrap()["job"]
            .clone();
        let request = mock.request().await;
        let headers = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer new-fixture-key"));
        assert!(!headers.contains("fixture-images-secret"));
        assert_eq!(
            f.terminal(queued["id"].as_str().unwrap()).await["status"],
            "completed"
        );
        images.shutdown().await;
        f.images.shutdown().await;
    }

    #[tokio::test]
    async fn editing_rechecks_identity_after_source_preparation_before_sending_http() {
        let bytes = png(true, 2, 2);
        let mut mock = Mock::new(vec![success(&bytes)]).await;
        let f = Fixture::new(&mock.base);
        let source = f.source(bytes);
        let (entered, mut received) = mpsc::unbounded_channel();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let waiting = barrier.clone();
        let read = f.images.inner.read_image.clone();
        let (out, _) = mpsc::unbounded_channel();
        let images = Images::with_config(
            f.images.inner.options.clone(),
            f.images.inner.credentials.clone(),
            Arc::new(move |id| {
                let _ = entered.send(());
                waiting.wait();
                read(id)
            }),
            f.images.inner.save_image.clone(),
            f.images.inner.http.clone(),
            out,
        );
        let queued = images
            .handle(
                "images/edit",
                &json!({"prompt":"Wait while preparing source","imageIds":[source]}),
            )
            .await
            .unwrap()
            .unwrap()["job"]
            .clone();
        tokio::time::timeout(Duration::from_secs(2), received.recv())
            .await
            .unwrap()
            .unwrap();
        *f.credentials.lock().unwrap() =
            (Auth::ApiKey("new-fixture-key".into()), "revision-B".into());
        barrier.wait();
        let job = f.terminal(queued["id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "interrupted");
        assert_eq!(job["attachmentIds"], json!([]));
        assert!(mock.requests.try_recv().is_err());
        assert_eq!(f.assets.lock().unwrap().len(), 1);
        images.shutdown().await;
        f.images.shutdown().await;
    }
}
