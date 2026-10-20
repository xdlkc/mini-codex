//! API-key-only Realtime WebRTC signaling and short audio transcription.
//! The frontend owns media devices and RTCPeerConnection; credentials stay here.

use crate::auth::Auth;
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::multipart::{Form, Part};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;

const DEFAULT_MODEL: &str = "gpt-realtime-2.1";
const TRANSCRIPTION_MODEL: &str = "gpt-4o-mini-transcribe";
const VOICES: [&str; 10] = [
    "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse", "marin", "cedar",
];
const MAX_SDP: usize = 256 * 1024;
const MAX_AUDIO: usize = 10 * 1024 * 1024;
const MAX_TRANSCRIPT: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, String>;
type Credentials = Arc<dyn Fn() -> Result<Auth> + Send + Sync>;

struct Options {
    base: String,
    create_timeout: Duration,
    cleanup_timeout: Duration,
    transcription_timeout: Duration,
}

#[derive(Default)]
struct Registry {
    generation: u64,
    closed: bool,
    calls: HashMap<String, Arc<Call>>,
    ended: HashSet<String>,
    ended_order: VecDeque<String>,
    jobs: Vec<JoinHandle<()>>,
}

#[derive(Default)]
struct CallState {
    cancelled: bool,
    provider_id: Option<String>,
    cleanup_started: bool,
    cleanup_error: Option<String>,
    finished: bool,
    result: Option<oneshot::Sender<Result<Value>>>,
}

struct Call {
    id: String,
    thread_id: String,
    key: String,
    state: Mutex<CallState>,
    changed: Notify,
}

struct Inner {
    registry: Mutex<Registry>,
    options: Options,
    http: reqwest::Client,
    credentials: Credentials,
}

pub struct Voice {
    inner: Arc<Inner>,
}

pub fn capabilities() -> Value {
    json!({
        "realtime": {"implemented":true,"transport":"webrtc","requiresApiKey":true,"chatgptOAuthSupported":false,"permissionsVerified":false},
        "dictation": {"implemented":true,"requiresApiKey":true,"model":TRANSCRIPTION_MODEL,"maxBytes":MAX_AUDIO,"permissionsVerified":false},
        "voices": VOICES, "defaultModel":DEFAULT_MODEL, "defaultVoice":"marin", "maxActiveCalls":4,
    })
}

impl Voice {
    pub fn new() -> Self {
        Self::with_config(
            Options {
                base: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                create_timeout: Duration::from_secs(20),
                cleanup_timeout: Duration::from_secs(5),
                transcription_timeout: Duration::from_secs(30),
            },
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static voice HTTP client configuration"),
            Arc::new(|| {
                Auth::load().map_err(|_| {
                    "Voice requires an OpenAI API key. Sign in with an API key first.".into()
                })
            }),
        )
    }

    fn with_config(options: Options, http: reqwest::Client, credentials: Credentials) -> Self {
        Self {
            inner: Arc::new(Inner {
                registry: Mutex::new(Registry::default()),
                options,
                http,
                credentials,
            }),
        }
    }

    pub fn capabilities(&self) -> Value {
        capabilities()
    }

    pub async fn start(&self, params: &Value) -> Result<Value> {
        let id = text(params, "id")?.to_string();
        let thread_id = text(params, "threadId")?.to_string();
        if !safe_id(&id) || thread_id.len() > 256 {
            return Err("Voice call and thread identifiers are invalid".into());
        }
        let sdp = text(params, "sdp")?.to_string();
        validate_sdp(&sdp)?;
        let model = params
            .get("model")
            .map(|v| v.as_str().ok_or("model must be a string"))
            .transpose()?
            .unwrap_or(DEFAULT_MODEL)
            .to_string();
        if model.is_empty()
            || model.len() > 100
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("Realtime model identifier is invalid".into());
        }
        let voice = params
            .get("voice")
            .map(|v| v.as_str().ok_or("voice must be a string"))
            .transpose()?
            .unwrap_or("marin")
            .to_string();
        if !VOICES.contains(&voice.as_str()) {
            return Err("Select a supported Realtime voice".into());
        }
        let instructions = params
            .get("instructions")
            .map(|v| v.as_str().ok_or("instructions must be a string"))
            .transpose()?
            .unwrap_or("")
            .to_string();
        if instructions.len() > 64 * 1024 {
            return Err("Voice instructions are too large".into());
        }
        endpoint(&self.inner.options.base, "realtime/calls")?;
        let key = api_key(&self.inner.credentials)?;
        let (tx, rx) = oneshot::channel();
        let call = Arc::new(Call {
            id: id.clone(),
            thread_id: thread_id.clone(),
            key,
            state: Mutex::new(CallState {
                result: Some(tx),
                ..CallState::default()
            }),
            changed: Notify::new(),
        });
        {
            let mut registry = self
                .inner
                .registry
                .lock()
                .map_err(|_| "Voice registry is unavailable")?;
            if registry.closed {
                return Err("Voice service is closed".into());
            }
            if registry.ended.contains(&id) {
                return Err("Voice call was already ended; use a new call identifier".into());
            }
            if registry.calls.contains_key(&id) {
                return Err("Voice call identifier is already in use".into());
            }
            // Pending cleanup retains its slot so repeated cancellation cannot
            // create an unbounded number of upstream initialization requests.
            if registry.calls.len() >= 4 {
                return Err("At most four voice calls may be active or awaiting cleanup".into());
            }
            if registry.calls.values().any(|other| {
                other.thread_id == thread_id
                    && other
                        .state
                        .lock()
                        .is_ok_and(|s| !s.cancelled && !s.finished)
            }) {
                return Err("This chat already has a voice call".into());
            }
            registry.calls.insert(id.clone(), call.clone());
        }
        let inner = self.inner.clone();
        let job = tokio::spawn(async move {
            let result = tokio::time::timeout(
                inner.options.create_timeout,
                negotiate(&inner, &call, &sdp, &model, &voice, &instructions),
            )
            .await
            .unwrap_or_else(|_| Err("Realtime signaling timed out".into()));
            let mut cleanup = false;
            {
                let mut state = call.state.lock().unwrap_or_else(|e| e.into_inner());
                if state.cancelled {
                    if let Some(tx) = state.result.take() {
                        let _ = tx.send(Err("Voice call was cancelled".into()));
                    }
                    cleanup = true;
                } else {
                    match result {
                        Ok(answer) => {
                            let value = json!({"id":call.id,"threadId":call.thread_id,"sdp":answer,"model":model,"voice":voice,"providerCallId":state.provider_id});
                            if state
                                .result
                                .take()
                                .is_none_or(|tx| tx.send(Ok(value)).is_err())
                            {
                                state.cancelled = true;
                                cleanup = true;
                            }
                        }
                        Err(error) => {
                            state.cancelled = true;
                            if let Some(tx) = state.result.take() {
                                let _ = tx.send(Err(error));
                            }
                            cleanup = true;
                        }
                    }
                }
            }
            if cleanup {
                cleanup_call(inner.clone(), call.clone()).await;
            }
        });
        track(&self.inner, job);
        rx.await
            .unwrap_or_else(|_| Err("Voice signaling ended before an answer was available".into()))
    }

    pub async fn end(&self, id: &str) -> Result<Value> {
        if !safe_id(id) {
            return Err("Voice call identifier is invalid".into());
        }
        let call = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .map_err(|_| "Voice registry is unavailable")?;
            remember_ended(&mut registry, id);
            registry.calls.get(id).cloned()
        };
        let Some(call) = call else {
            return Ok(json!({"id":id,"ended":true,"pending":false}));
        };
        let has_provider = cancel(&call);
        if has_provider {
            spawn_cleanup(self.inner.clone(), call.clone());
            wait_cleanup(
                &call,
                self.inner.options.cleanup_timeout + Duration::from_millis(500),
            )
            .await?;
        }
        let state = call
            .state
            .lock()
            .map_err(|_| "Voice call state is unavailable")?;
        if let Some(error) = &state.cleanup_error {
            return Err(error.clone());
        }
        Ok(json!({"id":id,"ended":true,"pending":!state.finished}))
    }

    pub async fn transcribe(&self, params: &Value) -> Result<Value> {
        let encoded = text(params, "dataBase64")?;
        if encoded.len() > MAX_AUDIO.div_ceil(3) * 4 {
            return Err("Audio exceeds the 10 MiB limit".into());
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| "Audio must be valid base64")?;
        if bytes.is_empty() || bytes.len() > MAX_AUDIO {
            return Err("Audio must contain between 1 byte and 10 MiB".into());
        }
        let supplied_mime = text(params, "mimeType")?;
        let mime = supplied_mime
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        let extension = match mime.as_str() {
            "audio/webm" => "webm",
            "audio/mp4" | "audio/m4a" => "m4a",
            "audio/mpeg" | "audio/mp3" => "mp3",
            "audio/wav" | "audio/x-wav" => "wav",
            "audio/ogg" => "ogg",
            "audio/flac" => "flac",
            _ => return Err("Unsupported audio MIME type".into()),
        };
        let default_name = format!("recording.{extension}");
        let filename = params
            .get("filename")
            .map(|v| v.as_str().ok_or("filename must be a string"))
            .transpose()?
            .unwrap_or(&default_name);
        if filename.is_empty()
            || filename.len() > 128
            || !filename
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            || filename == "."
            || filename == ".."
        {
            return Err("Audio filename must be a simple filename without path components".into());
        }
        let url = endpoint(&self.inner.options.base, "audio/transcriptions")?;
        let key = api_key(&self.inner.credentials)?;
        let generation = {
            let registry = self
                .inner
                .registry
                .lock()
                .map_err(|_| "Voice registry is unavailable")?;
            if registry.closed {
                return Err("Voice service is closed".into());
            }
            registry.generation
        };
        let file = Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(&mime)
            .map_err(|_| "Unsupported audio MIME type")?;
        let form = Form::new()
            .text("model", TRANSCRIPTION_MODEL)
            .text("response_format", "json")
            .part("file", file);
        let result = tokio::time::timeout(self.inner.options.transcription_timeout, async {
            let response = self
                .inner
                .http
                .post(url)
                .bearer_auth(key)
                .multipart(form)
                .send()
                .await
                .map_err(|_| "Audio transcription connection failed")?;
            if !response.status().is_success() {
                return Err(format!(
                    "Audio transcription failed (HTTP {})",
                    response.status().as_u16()
                ));
            }
            let body = bounded_body(response, MAX_TRANSCRIPT).await?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|_| "Audio transcription returned an invalid response")?;
            let transcript = value["text"]
                .as_str()
                .ok_or("Audio transcription returned no text")?;
            Ok::<Value, String>(json!({"text":transcript,"model":TRANSCRIPTION_MODEL}))
        })
        .await
        .unwrap_or_else(|_| Err("Audio transcription timed out".into()))?;
        let registry = self
            .inner
            .registry
            .lock()
            .map_err(|_| "Voice registry is unavailable")?;
        if registry.closed || registry.generation != generation {
            return Err("Audio transcription was cancelled".into());
        }
        Ok(result)
    }

    /// Cancel this generation without disabling future sessions after a new login.
    pub async fn shutdown(&self) {
        let calls = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            registry.generation = registry.generation.wrapping_add(1);
            let calls = registry.calls.values().cloned().collect::<Vec<_>>();
            for call in &calls {
                remember_ended(&mut registry, &call.id);
            }
            calls
        };
        for call in &calls {
            if cancel(call) {
                spawn_cleanup(self.inner.clone(), call.clone());
            }
        }
        let deadline = self.inner.options.create_timeout
            + self.inner.options.cleanup_timeout
            + Duration::from_secs(1);
        let waits = calls.iter().map(|call| wait_cleanup(call, deadline));
        let _ = futures_util::future::join_all(waits).await;
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        let calls = {
            let mut registry = self
                .inner
                .registry
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            registry.closed = true;
            registry.generation = registry.generation.wrapping_add(1);
            registry.calls.values().cloned().collect::<Vec<_>>()
        };
        for call in calls {
            if cancel(&call) && tokio::runtime::Handle::try_current().is_ok() {
                spawn_cleanup(self.inner.clone(), call);
            }
        }
        // Do not abort pending create requests: their owned workers must obtain
        // the upstream call id and hang up a late success even after this Drop.
    }
}

fn cancel(call: &Arc<Call>) -> bool {
    let mut state = call.state.lock().unwrap_or_else(|e| e.into_inner());
    state.cancelled = true;
    if !state.cleanup_started {
        state.cleanup_error = None;
    }
    if let Some(tx) = state.result.take() {
        let _ = tx.send(Err("Voice call was cancelled".into()));
    }
    call.changed.notify_waiters();
    state.provider_id.is_some() && !state.finished
}
fn remember_ended(registry: &mut Registry, id: &str) {
    if registry.ended.insert(id.into()) {
        registry.ended_order.push_back(id.into());
    }
    while registry.ended_order.len() > 256 {
        if let Some(old) = registry.ended_order.pop_front() {
            registry.ended.remove(&old);
        }
    }
}
fn track(inner: &Arc<Inner>, job: JoinHandle<()>) {
    let mut registry = inner.registry.lock().unwrap_or_else(|e| e.into_inner());
    registry.jobs.retain(|job| !job.is_finished());
    registry.jobs.push(job);
}
fn spawn_cleanup(inner: Arc<Inner>, call: Arc<Call>) {
    let owner = inner.clone();
    let job = tokio::spawn(async move {
        cleanup_call(owner, call).await;
    });
    track(&inner, job);
}
async fn cleanup_call(inner: Arc<Inner>, call: Arc<Call>) {
    let provider = {
        let mut state = call.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.finished || state.cleanup_started {
            return;
        }
        if let Some(id) = state.provider_id.clone() {
            state.cleanup_started = true;
            state.cleanup_error = None;
            Some(id)
        } else {
            state.finished = true;
            None
        }
    };
    let result = if let Some(provider) = provider {
        let result = tokio::time::timeout(inner.options.cleanup_timeout, async {
            let url = endpoint(
                &inner.options.base,
                &format!("realtime/calls/{provider}/hangup"),
            )?;
            let response = inner
                .http
                .post(url)
                .bearer_auth(&call.key)
                .send()
                .await
                .map_err(|_| "Could not contact the provider to end this voice call")?;
            if response.status().is_success() || matches!(response.status().as_u16(), 404 | 410) {
                Ok(())
            } else {
                Err(format!(
                    "Could not end the provider voice call (HTTP {})",
                    response.status().as_u16()
                ))
            }
        })
        .await
        .unwrap_or_else(|_| Err("Ending the provider voice call timed out".into()));
        let mut state = call.state.lock().unwrap_or_else(|e| e.into_inner());
        state.cleanup_started = false;
        if let Err(error) = &result {
            state.cleanup_error = Some(error.clone());
        } else {
            state.finished = true;
        }
        result
    } else {
        Ok(())
    };
    if result.is_ok() {
        let mut registry = inner.registry.lock().unwrap_or_else(|e| e.into_inner());
        if registry
            .calls
            .get(&call.id)
            .is_some_and(|current| Arc::ptr_eq(current, &call))
        {
            registry.calls.remove(&call.id);
            remember_ended(&mut registry, &call.id);
        }
    }
    call.changed.notify_waiters();
}
async fn wait_cleanup(call: &Arc<Call>, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            let notified = call.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = call
                    .state
                    .lock()
                    .map_err(|_| "Voice call state is unavailable")?;
                if state.finished {
                    return Ok(());
                }
                if !state.cleanup_started {
                    if let Some(error) = &state.cleanup_error {
                        return Err(error.clone());
                    }
                }
            }
            notified.await;
        }
    })
    .await
    .unwrap_or_else(|_| Err("Voice cleanup is still pending".into()))
}

async fn negotiate(
    inner: &Arc<Inner>,
    call: &Arc<Call>,
    sdp: &str,
    model: &str,
    voice: &str,
    instructions: &str,
) -> Result<String> {
    let session = json!({"type":"realtime","model":model,"audio":{"input":{"transcription":{"model":TRANSCRIPTION_MODEL},"turn_detection":{"type":"server_vad","create_response":true,"interrupt_response":true}},"output":{"voice":voice}},"tools":[],"instructions":instructions});
    let form = Form::new()
        .part(
            "sdp",
            Part::text(sdp.to_string())
                .mime_str("application/sdp")
                .unwrap(),
        )
        .part(
            "session",
            Part::text(session.to_string())
                .mime_str("application/json")
                .unwrap(),
        );
    let response = inner
        .http
        .post(endpoint(&inner.options.base, "realtime/calls")?)
        .bearer_auth(&call.key)
        .multipart(form)
        .send()
        .await
        .map_err(|_| "Realtime signaling connection failed")?;
    if !response.status().is_success() {
        return Err(format!(
            "Realtime signaling failed (HTTP {})",
            response.status().as_u16()
        ));
    }
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or("Realtime provider returned no usable call identifier")?;
    let provider = provider_id(location, &inner.options.base)?;
    {
        let mut state = call
            .state
            .lock()
            .map_err(|_| "Voice call state is unavailable")?;
        state.provider_id = Some(provider);
        if state.cancelled {
            return Err("Voice call was cancelled".into());
        }
    }
    let bytes = bounded_body(response, MAX_SDP).await?;
    let answer = String::from_utf8(bytes).map_err(|_| "Realtime provider returned invalid SDP")?;
    validate_sdp(&answer).map_err(|_| "Realtime provider returned invalid SDP")?;
    Ok(answer)
}
fn provider_id(location: &str, base: &str) -> Result<String> {
    let base = reqwest::Url::parse(base).map_err(|_| "Configured API URL is invalid")?;
    let url = base
        .join(location)
        .map_err(|_| "Realtime provider returned an invalid call identifier")?;
    if url.origin() != base.origin()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Realtime provider returned an invalid call identifier".into());
    }
    let prefix = format!("{}/realtime/calls/", base.path().trim_end_matches('/'));
    let id = url
        .path()
        .strip_prefix(&prefix)
        .filter(|id| safe_id(id))
        .ok_or("Realtime provider returned an invalid call identifier")?;
    Ok(id.into())
}
fn endpoint(base: &str, suffix: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(&format!("{}/{}", base.trim_end_matches('/'), suffix))
        .map_err(|_| "Configured API URL is invalid")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Configured API URL must use HTTP(S) without embedded credentials, query, or fragment"
                .into(),
        );
    }
    Ok(url)
}
async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Provider response exceeds the size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read provider response")?
    {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err("Provider response exceeds the size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn validate_sdp(sdp: &str) -> Result<()> {
    if sdp.is_empty()
        || sdp.len() > MAX_SDP
        || !sdp.starts_with("v=0")
        || !sdp.lines().any(|line| line.starts_with("m=audio "))
    {
        Err("Audio SDP must be valid and at most 256 KiB".into())
    } else {
        Ok(())
    }
}
fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}
fn api_key(credentials: &Credentials) -> Result<String> {
    match credentials()?{Auth::ApiKey(key) if !key.trim().is_empty()=>Ok(key),Auth::ChatGpt{..}=>Err("Realtime voice and dictation require an OpenAI API key; ChatGPT OAuth is not supported by this API service".into()),_=>Err("Voice requires an OpenAI API key. Sign in with an API key first.".into())}
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const SDP:&str="v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=local-test\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n";
    struct Response {
        status: u16,
        location: Option<String>,
        body: Vec<u8>,
        declared: Option<usize>,
        release: Option<oneshot::Receiver<()>>,
    }
    impl Response {
        fn sdp(id: &str) -> Self {
            Self {
                status: 201,
                location: Some(format!("/v1/realtime/calls/{id}")),
                body: SDP.as_bytes().to_vec(),
                declared: None,
                release: None,
            }
        }
        fn empty() -> Self {
            Self {
                status: 204,
                location: None,
                body: vec![],
                declared: None,
                release: None,
            }
        }
    }
    struct Mock {
        base: String,
        requests: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
        task: JoinHandle<()>,
    }
    impl Mock {
        async fn new(responses: Vec<Response>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/v1", listener.local_addr().unwrap());
            let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                for response in responses {
                    let (mut socket, _) =
                        tokio::time::timeout(Duration::from_secs(3), listener.accept())
                            .await
                            .unwrap()
                            .unwrap();
                    let mut data = vec![];
                    loop {
                        let mut part = [0; 4096];
                        let n = socket.read(&mut part).await.unwrap();
                        assert!(n > 0);
                        data.extend_from_slice(&part[..n]);
                        if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                            let header = String::from_utf8_lossy(&data[..end]);
                            let length = header
                                .lines()
                                .filter_map(|line| line.split_once(':'))
                                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                .map(|(_, n)| n.trim().parse::<usize>().unwrap())
                                .unwrap_or(0);
                            if data.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let _ = tx.send(data);
                    if let Some(release) = response.release {
                        let _ = tokio::time::timeout(Duration::from_secs(3), release).await;
                    }
                    let location = response
                        .location
                        .map(|s| format!("Location: {s}\r\n"))
                        .unwrap_or_default();
                    let header=format!("HTTP/1.1 {} Test\r\nContent-Length: {}\r\n{location}Connection: close\r\n\r\n",response.status,response.declared.unwrap_or(response.body.len()));
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&response.body).await;
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
    fn service(base: &str) -> Voice {
        Voice::with_config(
            Options {
                base: base.into(),
                create_timeout: Duration::from_secs(2),
                cleanup_timeout: Duration::from_secs(1),
                transcription_timeout: Duration::from_secs(2),
            },
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            Arc::new(|| Ok(Auth::ApiKey("fixture-voice-key".into()))),
        )
    }
    fn params(id: &str, thread: &str) -> Value {
        json!({"id":id,"threadId":thread,"sdp":SDP,"instructions":"Fixture trusted instructions"})
    }
    async fn empty_calls(voice: &Voice) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if voice.inner.registry.lock().unwrap().calls.is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unified_signaling_uses_multipart_session_and_idempotent_hangup() {
        let mut mock = Mock::new(vec![Response::sdp("rtc_fixture"), Response::empty()]).await;
        let voice = service(&mock.base);
        let result = voice.start(&params("call-1", "thread-1")).await.unwrap();
        let request = String::from_utf8(mock.request().await).unwrap();
        assert!(request.starts_with("POST /v1/realtime/calls "));
        assert!(request.contains("Bearer fixture-voice-key"));
        assert!(request.contains("name=\"sdp\""));
        assert!(request.contains("application/sdp"));
        assert!(request.contains("name=\"session\""));
        assert!(request.contains("application/json"));
        for text in [
            "gpt-realtime-2.1",
            "gpt-4o-mini-transcribe",
            "marin",
            "server_vad",
            "\"create_response\":true",
            "\"interrupt_response\":true",
            "\"tools\":[]",
            "Fixture trusted instructions",
        ] {
            assert!(request.contains(text), "missing {text}");
        }
        assert_eq!(result["sdp"], SDP);
        assert_eq!(result["providerCallId"], "rtc_fixture");
        assert!(!result.to_string().contains("fixture-voice-key"));
        assert_eq!(voice.end("call-1").await.unwrap()["ended"], true);
        assert!(String::from_utf8(mock.request().await)
            .unwrap()
            .starts_with("POST /v1/realtime/calls/rtc_fixture/hangup "));
        assert_eq!(voice.end("call-1").await.unwrap()["ended"], true);
        voice.shutdown().await;
    }
    #[tokio::test]
    async fn ending_pending_start_hangs_up_the_late_provider_call() {
        let (tx, release) = oneshot::channel();
        let mut response = Response::sdp("rtc_late");
        response.release = Some(release);
        let mut mock = Mock::new(vec![response, Response::empty()]).await;
        let voice = Arc::new(service(&mock.base));
        let start = voice.clone();
        let task = tokio::spawn(async move { start.start(&params("late", "thread")).await });
        mock.request().await;
        assert_eq!(voice.end("late").await.unwrap()["pending"], true);
        assert!(task.await.unwrap().is_err());
        let _ = tx.send(());
        assert!(String::from_utf8(mock.request().await)
            .unwrap()
            .contains("/rtc_late/hangup"));
        empty_calls(&voice).await;
        voice.shutdown().await;
    }
    #[tokio::test]
    async fn drop_and_abandoned_start_still_cleanup_a_late_success() {
        let (tx, release) = oneshot::channel();
        let mut response = Response::sdp("rtc_dropped");
        response.release = Some(release);
        let mut mock = Mock::new(vec![response, Response::empty()]).await;
        let voice = Arc::new(service(&mock.base));
        let owned = voice.clone();
        let task = tokio::spawn(async move { owned.start(&params("drop", "thread")).await });
        mock.request().await;
        task.abort();
        let _ = task.await;
        drop(voice);
        let _ = tx.send(());
        assert!(String::from_utf8(mock.request().await)
            .unwrap()
            .contains("/rtc_dropped/hangup"));
    }
    #[tokio::test]
    async fn malicious_location_is_never_followed_and_oversized_answer_is_cleaned_up() {
        let trap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut bad = Response::sdp("rtc_bad");
        bad.location = Some(format!(
            "http://{}/v1/realtime/calls/rtc_bad",
            trap.local_addr().unwrap()
        ));
        let mut mock = Mock::new(vec![bad]).await;
        let voice = service(&mock.base);
        let error = voice.start(&params("bad", "thread")).await.unwrap_err();
        assert!(!error.contains(SDP));
        assert!(!error.contains("fixture-voice-key"));
        mock.request().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), trap.accept())
                .await
                .is_err()
        );
        empty_calls(&voice).await;
        let mut big = Response::sdp("rtc_big");
        big.declared = Some(MAX_SDP + 1);
        big.body.clear();
        let mut mock = Mock::new(vec![big, Response::empty()]).await;
        let voice = service(&mock.base);
        assert!(voice.start(&params("big", "thread")).await.is_err());
        mock.request().await;
        assert!(String::from_utf8(mock.request().await)
            .unwrap()
            .contains("/rtc_big/hangup"));
        empty_calls(&voice).await;
    }
    #[tokio::test]
    async fn capacity_and_per_thread_limits_are_enforced_before_network() {
        let responses = (0..4)
            .map(|i| Response::sdp(&format!("rtc_{i}")))
            .chain((0..4).map(|_| Response::empty()))
            .collect();
        let mut mock = Mock::new(responses).await;
        let voice = service(&mock.base);
        voice.start(&params("call-0", "thread-0")).await.unwrap();
        mock.request().await;
        assert!(voice.start(&params("duplicate", "thread-0")).await.is_err());
        for index in 1..4 {
            voice
                .start(&params(
                    &format!("call-{index}"),
                    &format!("thread-{index}"),
                ))
                .await
                .unwrap();
            mock.request().await;
        }
        assert!(voice.start(&params("fifth", "thread-5")).await.is_err());
        voice.shutdown().await;
        for _ in 0..4 {
            assert!(String::from_utf8(mock.request().await)
                .unwrap()
                .contains("/hangup"));
        }
        empty_calls(&voice).await;
    }
    #[tokio::test]
    async fn dictation_posts_fixed_model_audio_bytes_and_returns_only_transcript() {
        let response = Response {
            status: 200,
            location: None,
            body: serde_json::to_vec(&json!({"text":"你好，语音转写"})).unwrap(),
            declared: None,
            release: None,
        };
        let mut mock = Mock::new(vec![response]).await;
        let voice = service(&mock.base);
        let result=voice.transcribe(&json!({"dataBase64":STANDARD.encode(b"fixture-audio"),"mimeType":"audio/webm;codecs=opus"})).await.unwrap();
        let body = String::from_utf8(mock.request().await).unwrap();
        assert!(body.starts_with("POST /v1/audio/transcriptions "));
        assert!(body.contains("fixture-audio"));
        assert!(body.contains("gpt-4o-mini-transcribe"));
        assert!(body.contains("recording.webm"));
        assert_eq!(
            result,
            json!({"text":"你好，语音转写","model":TRANSCRIPTION_MODEL})
        );
        for data in [
            json!({"dataBase64":"invalid!","mimeType":"audio/webm"}),
            json!({"dataBase64":STANDARD.encode(b"a"),"mimeType":"text/plain"}),
            json!({"dataBase64":STANDARD.encode(b"a"),"mimeType":"audio/webm","filename":"../secret.webm"}),
            json!({"dataBase64":STANDARD.encode(vec![0;MAX_AUDIO+1]),"mimeType":"audio/webm"}),
        ] {
            assert!(voice.transcribe(&data).await.is_err());
        }
    }
    #[tokio::test]
    async fn shutdown_discards_late_transcription_and_service_remains_reusable() {
        let (tx, release) = oneshot::channel();
        let response = Response {
            status: 200,
            location: None,
            body: br#"{"text":"late private transcript"}"#.to_vec(),
            declared: None,
            release: Some(release),
        };
        let mut mock = Mock::new(vec![response, Response::sdp("rtc_new"), Response::empty()]).await;
        let voice = Arc::new(service(&mock.base));
        let owned = voice.clone();
        let task = tokio::spawn(async move {
            owned
                .transcribe(&json!({"dataBase64":STANDARD.encode(b"audio"),"mimeType":"audio/wav"}))
                .await
        });
        mock.request().await;
        voice.shutdown().await;
        let _ = tx.send(());
        let error = task.await.unwrap().unwrap_err();
        assert!(!error.contains("late private transcript"));
        voice.start(&params("new", "thread")).await.unwrap();
        mock.request().await;
        voice.end("new").await.unwrap();
        mock.request().await;
    }
    #[tokio::test]
    async fn oauth_is_explicitly_unavailable_and_errors_never_reflect_provider_body() {
        let mut voice = service("http://127.0.0.1:1/v1");
        Arc::get_mut(&mut voice.inner).unwrap().credentials = Arc::new(|| {
            Ok(Auth::ChatGpt {
                access: "oauth-secret".into(),
                refresh: "refresh-secret".into(),
                account_id: "account".into(),
            })
        });
        let error = voice.start(&params("oauth", "thread")).await.unwrap_err();
        assert!(error.contains("OAuth"));
        assert!(!error.contains("oauth-secret"));
        assert_eq!(
            voice.capabilities()["realtime"]["permissionsVerified"],
            false
        );
        assert_eq!(voice.capabilities()["voices"].as_array().unwrap().len(), 10);
        let response = Response {
            status: 401,
            location: None,
            body: b"fixture-voice-key private SDP provider error".to_vec(),
            declared: None,
            release: None,
        };
        let mut mock = Mock::new(vec![response]).await;
        let voice = service(&mock.base);
        let error = voice.start(&params("reject", "thread")).await.unwrap_err();
        assert!(error.contains("401"));
        assert!(!error.contains("fixture-voice-key"));
        assert!(!error.contains("private SDP"));
        mock.request().await;
        empty_calls(&voice).await;
    }
    #[tokio::test]
    async fn end_before_start_is_idempotent_and_reserves_the_cancelled_id() {
        let voice = service("http://127.0.0.1:1/v1");
        assert_eq!(voice.end("cancelled").await.unwrap()["ended"], true);
        assert!(voice.start(&params("cancelled", "thread")).await.is_err());
        assert!(provider_id(
            "/v1/realtime/calls/rtc_%2fsecret",
            "https://api.openai.com/v1"
        )
        .is_err());
        assert!(provider_id(
            "https://other.invalid/v1/realtime/calls/rtc_abc",
            "https://api.openai.com/v1"
        )
        .is_err());
    }

    #[tokio::test]
    async fn failed_hangup_preserves_the_call_identifier_for_a_real_retry() {
        let failed = Response {
            status: 503,
            location: None,
            body: b"fixture-voice-key private provider body".to_vec(),
            declared: None,
            release: None,
        };
        let mut mock = Mock::new(vec![Response::sdp("rtc_retry"), failed, Response::empty()]).await;
        let voice = service(&mock.base);
        voice.start(&params("retry", "thread")).await.unwrap();
        mock.request().await;
        let error = voice.end("retry").await.unwrap_err();
        assert!(error.contains("503"));
        assert!(!error.contains("fixture-voice-key"));
        mock.request().await;
        assert_eq!(voice.end("retry").await.unwrap()["ended"], true);
        assert!(String::from_utf8(mock.request().await)
            .unwrap()
            .contains("/rtc_retry/hangup"));
        empty_calls(&voice).await;
    }

    #[tokio::test]
    async fn signaling_body_timeout_still_hangs_up_the_known_provider_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut received = vec![];
            loop {
                let mut bytes = [0; 4096];
                let n = first.read(&mut bytes).await.unwrap();
                assert!(n > 0);
                received.extend_from_slice(&bytes[..n]);
                if let Some(end) = received.windows(4).position(|s| s == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&received[..end]);
                    let length = header
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, length)| length.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if received.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            first.write_all(format!("HTTP/1.1 201 Created\r\nLocation: /v1/realtime/calls/rtc_timeout\r\nContent-Length: {}\r\n\r\n",SDP.len()).as_bytes()).await.unwrap();
            // Headers establish the provider id; the SDP body intentionally stalls.
            let (mut hangup, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = [0; 4096];
            let n = hangup.read(&mut bytes).await.unwrap();
            assert!(String::from_utf8_lossy(&bytes[..n])
                .starts_with("POST /v1/realtime/calls/rtc_timeout/hangup "));
            hangup
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let mut voice = service(&base);
        Arc::get_mut(&mut voice.inner)
            .unwrap()
            .options
            .create_timeout = Duration::from_millis(100);
        assert!(voice
            .start(&params("timeout", "thread"))
            .await
            .unwrap_err()
            .contains("timed out"));
        empty_calls(&voice).await;
        server.await.unwrap();
    }
}
