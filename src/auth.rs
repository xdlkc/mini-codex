//! Owned credentials, read-only Codex import, and OAuth PKCE.
//! Desktop OAuth neither opens a browser nor writes to stdout.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
static STORE_LOCK: Mutex<()> = Mutex::new(());
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    Unavailable,
    ApiKey(String),
    ChatGpt {
        access: String,
        refresh: String,
        account_id: String,
    },
}
impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Unavailable",
            Self::ApiKey(_) => "ApiKey([redacted])",
            Self::ChatGpt { .. } => "ChatGpt([redacted])",
        })
    }
}
#[derive(Clone)]
enum Environment {
    Process,
    Fixed(Option<String>),
}
#[derive(Clone)]
pub(crate) struct Store {
    directory: PathBuf,
    imported: Option<PathBuf>,
    environment: Environment,
}
pub(crate) struct Snapshot {
    pub auth: Auth,
    pub source: Option<&'static str>,
    pub revision: String,
    id_token: String,
}
impl Store {
    pub fn configured() -> Self {
        let imported = crate::config::codex_import_enabled().then(||crate::config::codex_home().join("auth.json"));
        Self {
            directory: crate::config::home(),
            imported,
            environment: Environment::Process,
        }
    }
    #[cfg(test)]
    pub(crate) fn isolated(
        directory: PathBuf,
        imported: Option<PathBuf>,
        environment_key: Option<String>,
    ) -> Self {
        Self {
            directory,
            imported,
            environment: Environment::Fixed(environment_key),
        }
    }
    pub(crate) fn environment_key(&self) -> Option<String> {
        match &self.environment {
            Environment::Process => std::env::var("OPENAI_API_KEY").ok(),
            Environment::Fixed(key) => key.clone(),
        }
        .filter(|key| !key.trim().is_empty())
    }
    fn path(&self) -> PathBuf {
        self.directory.join("auth.json")
    }
    fn locked<T>(&self, action: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| anyhow!("Credential lock is unavailable"))?;
        fs::create_dir_all(&self.directory).context("creating credential directory")?;
        let path = self.directory.join("auth.lock");
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            bail!("Credential lock must not be a symlink");
        }
        let lock = private_options()
            .create(true)
            .read(true)
            .write(true)
            .open(path)
            .context("opening credential lock")?;
        lock.lock().context("locking credentials")?;
        action()
    }
    fn snapshot_unlocked(&self) -> Result<Snapshot> {
        if let Some(key) = self.environment_key() {
            return Ok(Snapshot {
                revision: format!("environment:{}", digest(key.as_bytes())),
                auth: Auth::ApiKey(key),
                source: Some("environment"),
                id_token: String::new(),
            });
        }
        match fs::read(self.path()) {
            Ok(bytes) => {
                let doc = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                let revision = doc["revision"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("local:{}", digest(&bytes)));
                // An owned signed-out or unreadable file never falls back to another app's account.
                let auth = if doc["signed_out"] == true || doc["auth_mode"] == "signed_out" {
                    Auth::Unavailable
                } else {
                    parse_file(&doc).unwrap_or(Auth::Unavailable)
                };
                let source = (!matches!(auth, Auth::Unavailable)).then_some("local");
                return Ok(Snapshot {
                    auth,
                    source,
                    revision,
                    id_token: doc["tokens"]["id_token"].as_str().unwrap_or("").into(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("reading owned credentials"),
        }
        if let Some(path) = &self.imported {
            match fs::read(path) {
                Ok(bytes) => {
                    let doc = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
                    if let Some(auth) = parse_file(&doc) {
                        return Ok(Snapshot {
                            auth,
                            source: Some("imported"),
                            revision: format!("imported:{}", digest(&bytes)),
                            id_token: doc["tokens"]["id_token"].as_str().unwrap_or("").into(),
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).context("reading imported credentials"),
            }
        }
        Ok(Snapshot {
            auth: Auth::Unavailable,
            source: None,
            revision: "absent".into(),
            id_token: String::new(),
        })
    }
    pub(crate) fn snapshot(&self) -> Result<Snapshot> {
        if self.environment_key().is_some() {
            return self.snapshot_unlocked();
        }
        self.locked(|| self.snapshot_unlocked())
    }
    pub(crate) fn metadata(&self) -> Result<Value> {
        let snap = self.snapshot()?;
        let account = match &snap.auth {
            Auth::Unavailable => Value::Null,
            Auth::ApiKey(_) => json!({"type":"apiKey"}),
            Auth::ChatGpt { access, .. } => {
                let c = jwt_claims(&snap.id_token)
                    .or_else(|| jwt_claims(access))
                    .unwrap_or(Value::Null);
                json!({"type":"chatgpt","email":c["email"].as_str(),"planType":c["https://api.openai.com/auth"]["chatgpt_plan_type"].as_str()})
            }
        };
        Ok(
            json!({"account":account,"credentialSource":snap.source,"canChangeCredentials":snap.source!=Some("environment"),"requiresOpenaiAuth":true}),
        )
    }
    fn write_document(&self, doc: &Value) -> Result<()> {
        let temp =
            self.directory
                .join(format!(".auth-{}-{}.tmp", std::process::id(), rand_b64(18)));
        let result = (|| {
            let mut file = private_options()
                .write(true)
                .create_new(true)
                .open(&temp)
                .context("creating private credentials")?;
            file.write_all(&serde_json::to_vec_pretty(doc)?)
                .context("writing credentials")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, self.path()).context("replacing credentials")?;
            #[cfg(unix)]
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
    pub(crate) fn ensure_mutable(&self) -> Result<()> {
        if self.environment_key().is_some() {
            bail!("Credentials are managed by OPENAI_API_KEY. Remove that environment variable before changing accounts in the app.");
        }
        Ok(())
    }
    pub(crate) fn save_if_revision(
        &self,
        expected: &str,
        auth: &Auth,
        id_token: &str,
    ) -> Result<()> {
        self.locked(|| {
            self.ensure_mutable()?;
            if self.snapshot_unlocked()?.revision != expected {
                bail!("Credentials changed during sign-in; start sign-in again");
            }
            self.write_document(&auth_document(auth, id_token, &rand_b64(24)))
        })
    }
    fn save_unconditionally(&self, auth: &Auth, id_token: &str) -> Result<()> {
        self.locked(|| self.write_document(&auth_document(auth, id_token, &rand_b64(24))))
    }
    pub(crate) fn logout(&self) -> Result<bool> {
        self.locked(|| {
            let had_owned = fs::read(self.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .as_ref()
                .and_then(parse_file)
                .is_some();
            self.write_document(
                &json!({"auth_mode":"signed_out","signed_out":true,"revision":rand_b64(24)}),
            )?;
            Ok(had_owned)
        })
    }
    fn save_refresh(&self, expected: &Snapshot, updated: &Auth, id_token: &str) -> Result<bool> {
        self.locked(|| {
            let current = self.snapshot_unlocked()?;
            if current.source == Some("environment")
                || current.revision != expected.revision
                || current.auth != expected.auth
            {
                return Ok(false);
            }
            // A token rotation preserves the explicit account-generation identifier.
            self.write_document(&auth_document(updated, id_token, &expected.revision))?;
            Ok(true)
        })
    }
}
impl Auth {
    pub fn load() -> Result<Self> {
        let auth = Store::configured().snapshot()?.auth;
        if matches!(auth, Self::Unavailable) {
            bail!("not signed in: run `mcodex login` or set OPENAI_API_KEY");
        }
        Ok(auth)
    }
    pub fn base_url(&self) -> String {
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| match self {
            Self::ChatGpt { .. } => "https://chatgpt.com/backend-api/codex".into(),
            _ => "https://api.openai.com/v1".into(),
        })
    }
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Unavailable => "Not signed in",
            Self::ApiKey(_) => "API key",
            Self::ChatGpt { .. } => "ChatGPT",
        }
    }
    pub async fn ensure_fresh(&mut self, http: &reqwest::Client) -> Result<()> {
        if matches!(self, Self::Unavailable) {
            *self = Self::load()?;
        }
        if matches!(self, Self::ApiKey(_)) {
            return Ok(());
        }
        self.ensure_fresh_at(http, &Store::configured(), &format!("{ISSUER}/oauth/token"))
            .await
    }
    async fn ensure_fresh_at(
        &mut self,
        http: &reqwest::Client,
        store: &Store,
        endpoint: &str,
    ) -> Result<()> {
        let Self::ChatGpt {
            access,
            refresh,
            account_id,
        } = self.clone()
        else {
            return Ok(());
        };
        let expiry = jwt_claims(&access)
            .and_then(|v| v["exp"].as_u64())
            .unwrap_or(0);
        if expiry > epoch_seconds().saturating_add(60) {
            return Ok(());
        }
        let before = store.snapshot()?;
        if before.auth != *self {
            *self = before.auth;
            bail!("Credentials changed before token refresh; retry with the current account");
        }
        let response=tokio::time::timeout(Duration::from_secs(20),http.post(endpoint).json(&json!({"client_id":CLIENT_ID,"grant_type":"refresh_token","refresh_token":refresh,"scope":"openid profile email"})).send()).await.context("Token refresh timed out")?.context("Token refresh failed")?;
        if !response.status().is_success() {
            bail!(
                "Token refresh failed (HTTP {}); sign in again",
                response.status().as_u16()
            );
        }
        let value: Value = tokio::time::timeout(Duration::from_secs(20), response.json())
            .await
            .context("Token refresh response timed out")??;
        let access = secret(&value, "access_token")?.to_string();
        let refresh = value["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&refresh)
            .to_string();
        let id_token = value["id_token"].as_str().unwrap_or(&before.id_token);
        if jwt_claims(id_token)
            .and_then(|v| {
                v["https://api.openai.com/auth"]["chatgpt_account_id"]
                    .as_str()
                    .map(String::from)
            })
            .is_some_and(|id| id != account_id)
        {
            bail!("Token refresh returned a different account");
        }
        let updated = Self::ChatGpt {
            access,
            refresh,
            account_id,
        };
        if !store.save_refresh(&before, &updated, id_token)? {
            *self = store.snapshot()?.auth;
            bail!("Credentials changed during token refresh; the stale result was discarded");
        }
        *self = updated;
        Ok(())
    }
}
fn auth_document(auth: &Auth, id_token: &str, revision: &str) -> Value {
    match auth {
        Auth::Unavailable => {
            json!({"auth_mode":"signed_out","signed_out":true,"revision":revision})
        }
        Auth::ApiKey(key) => json!({"auth_mode":"apikey","OPENAI_API_KEY":key,"revision":revision}),
        Auth::ChatGpt {
            access,
            refresh,
            account_id,
        } => {
            json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,"revision":revision,"tokens":{"id_token":id_token,"access_token":access,"refresh_token":refresh,"account_id":account_id}})
        }
    }
}
fn parse_file(v: &Value) -> Option<Auth> {
    if let Some(key) = v["OPENAI_API_KEY"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
    {
        return Some(Auth::ApiKey(key.into()));
    }
    let t = &v["tokens"];
    Some(Auth::ChatGpt {
        access: t["access_token"].as_str().filter(|s| !s.is_empty())?.into(),
        refresh: t["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())?
            .into(),
        account_id: t["account_id"].as_str().filter(|s| !s.is_empty())?.into(),
    })
}
pub(crate) fn jwt_claims(token: &str) -> Option<Value> {
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?).ok()
}
fn secret<'a>(v: &'a Value, field: &str) -> Result<&'a str> {
    v[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Authentication response is missing {field}"))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn rand_b64(n: usize) -> String {
    let mut bytes = vec![0; n];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
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
pub(crate) fn validate_key(key: &str) -> Result<&str> {
    let key = key.trim();
    if key.is_empty() || key.len() > 8192 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("Enter a non-empty API key without spaces or control characters");
    }
    Ok(key)
}

pub(crate) struct OAuthAttempt {
    pub auth_url: String,
    pub redirect_uri: String,
    listener: tokio::net::TcpListener,
    verifier: String,
    state: String,
    token_endpoint: String,
}
impl OAuthAttempt {
    pub async fn start(issuer: &str, port: u16) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .context("Could not open OAuth callback listener; port 1455 may be in use")?;
        let redirect_uri = format!(
            "http://localhost:{}/auth/callback",
            listener.local_addr()?.port()
        );
        let verifier = rand_b64(48);
        let state = rand_b64(24);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url =
            reqwest::Url::parse(&format!("{}/oauth/authorize", issuer.trim_end_matches('/')))?;
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", redirect_uri.as_str()),
            ("scope", "openid profile email offline_access"),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("originator", "codex_cli_rs"),
            ("id_token_add_organizations", "true"),
            ("codex_cli_simplified_flow", "true"),
        ]);
        Ok(Self {
            auth_url: url.into(),
            redirect_uri,
            listener,
            verifier,
            state,
            token_endpoint: format!("{}/oauth/token", issuer.trim_end_matches('/')),
        })
    }
    pub async fn finish(self, http: &reqwest::Client) -> Result<(Auth, String)> {
        let code = self.callback().await?;
        let response = http
            .post(&self.token_endpoint)
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("client_id", CLIENT_ID),
                ("code_verifier", self.verifier.as_str()),
            ])
            .send()
            .await
            .context("OAuth token exchange failed")?;
        if !response.status().is_success() {
            bail!(
                "OAuth token exchange failed (HTTP {})",
                response.status().as_u16()
            );
        }
        let value: Value = response
            .json()
            .await
            .context("OAuth token response was not valid JSON")?;
        let id_token = secret(&value, "id_token")?.to_string();
        let account_id = jwt_claims(&id_token)
            .and_then(|v| {
                v["https://api.openai.com/auth"]["chatgpt_account_id"]
                    .as_str()
                    .map(String::from)
            })
            .ok_or_else(|| anyhow!("OAuth response did not include a ChatGPT account"))?;
        Ok((
            Auth::ChatGpt {
                access: secret(&value, "access_token")?.into(),
                refresh: secret(&value, "refresh_token")?.into(),
                account_id,
            },
            id_token,
        ))
    }
    async fn callback(&self) -> Result<String> {
        loop {
            let (mut socket, _) = self
                .listener
                .accept()
                .await
                .context("OAuth listener stopped")?;
            let received =
                tokio::time::timeout(Duration::from_secs(3), read_callback(&mut socket)).await;
            let target = match received {
                Ok(Ok(target)) => target,
                _ => {
                    let _ = reply(&mut socket, 400, "Invalid callback request").await;
                    continue;
                }
            };
            let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
                let _ = reply(&mut socket, 400, "Invalid callback request").await;
                continue;
            };
            if url.path() != "/auth/callback" {
                let _ = reply(&mut socket, 404, "Not found").await;
                continue;
            }
            let pairs: Vec<_> = url.query_pairs().collect();
            let values = |name: &str| {
                pairs
                    .iter()
                    .filter(|(key, _)| key == name)
                    .map(|(_, value)| value.as_ref())
                    .collect::<Vec<_>>()
            };
            let states = values("state");
            if states.len() != 1 || states[0] != self.state {
                let _ = reply(&mut socket, 400, "Invalid sign-in state").await;
                continue;
            }
            if !values("error").is_empty() {
                let _ = reply(&mut socket, 400, "Sign-in was not authorized").await;
                bail!("Sign-in was not authorized; existing credentials were kept");
            }
            let codes = values("code");
            if codes.len() != 1 || codes[0].is_empty() || codes[0].len() > 4096 {
                let _ = reply(&mut socket, 400, "Missing sign-in code").await;
                continue;
            }
            let code = codes[0].to_string();
            reply(
                &mut socket,
                200,
                "Authorization received. Return to the app to check sign-in status.",
            )
            .await?;
            return Ok(code);
        }
    }
}
async fn read_callback(socket: &mut tokio::net::TcpStream) -> Result<String> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0u8; 1024];
        let n = socket.read(&mut chunk).await?;
        if n == 0 {
            bail!("incomplete callback");
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() > 8192 {
            bail!("callback headers too large");
        }
        if bytes.windows(4).any(|p| p == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&bytes)?;
    let mut first = request.lines().next().unwrap_or("").split_whitespace();
    if first.next() != Some("GET") {
        bail!("callback must use GET");
    }
    let target = first
        .next()
        .filter(|s| s.starts_with('/') && !s.starts_with("//"))
        .ok_or_else(|| anyhow!("invalid callback target"))?;
    if !matches!(first.next(), Some("HTTP/1.1" | "HTTP/1.0")) || first.next().is_some() {
        bail!("invalid request line");
    }
    Ok(target.into())
}
async fn reply(socket: &mut tokio::net::TcpStream, status: u16, body: &str) -> Result<()> {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    socket.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n{body}",body.len()).as_bytes()).await?;
    Ok(())
}

pub async fn login() -> Result<()> {
    let store = Store::configured();
    store.ensure_mutable()?;
    let revision = store.snapshot()?.revision;
    let attempt = OAuthAttempt::start(ISSUER, 1455).await?;
    eprintln!("Open this URL to sign in:\n{}", attempt.auth_url);
    let _ = tokio::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(&attempt.auth_url)
    .status()
    .await;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let (auth, id_token) = tokio::time::timeout(Duration::from_secs(300), attempt.finish(&http))
        .await
        .context("Sign-in timed out; existing credentials were kept")??;
    store.save_if_revision(&revision, &auth, &id_token)?;
    eprintln!("Signed in. Credentials saved to {}", store.path().display());
    Ok(())
}
pub fn login_with_api_key(key: &str) -> Result<()> {
    let store = Store::configured();
    store.ensure_mutable()?;
    store.save_unconditionally(&Auth::ApiKey(validate_key(key)?.into()), "")
}
pub fn logout() -> Result<bool> {
    Store::configured().logout()
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("mcodex-auth-{}", rand_b64(18))))
        }
        fn store(&self) -> Store {
            Store::isolated(self.0.join("own"), Some(self.0.join("import.json")), None)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn logout_sentinel_blocks_import_and_atomic_files_are_private() {
        let f = Fixture::new();
        fs::create_dir_all(&f.0).unwrap();
        let imported = f.0.join("import.json");
        fs::write(&imported, br#"{"OPENAI_API_KEY":"imported-test-secret"}"#).unwrap();
        let original = fs::read(&imported).unwrap();
        let store = f.store();
        assert_eq!(store.snapshot().unwrap().source, Some("imported"));
        store.logout().unwrap();
        assert!(matches!(
            f.store().snapshot().unwrap().auth,
            Auth::Unavailable
        ));
        assert_eq!(fs::read(imported).unwrap(), original);
        let rev = store.snapshot().unwrap().revision;
        store
            .save_if_revision(&rev, &Auth::ApiKey("owned-test-secret".into()), "")
            .unwrap();
        let meta = store.metadata().unwrap().to_string();
        assert!(!meta.contains("owned-test-secret"));
        assert!(!meta.contains("imported-test-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(store.path()).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert_eq!(fs::read_dir(store.directory).unwrap().count(), 2);
    }
    #[test]
    fn environment_is_managed_and_stale_signin_cannot_override_new_account() {
        let f = Fixture::new();
        let store = f.store();
        let rev = store.snapshot().unwrap().revision;
        store
            .save_if_revision(&rev, &Auth::ApiKey("first-test-key".into()), "")
            .unwrap();
        assert!(store
            .save_if_revision(&rev, &Auth::ApiKey("stale-test-key".into()), "")
            .is_err());
        let managed = Store::isolated(
            store.directory.clone(),
            None,
            Some("environment-test-key".into()),
        );
        assert_eq!(managed.metadata().unwrap()["canChangeCredentials"], false);
        assert!(managed.ensure_mutable().is_err());
        assert_eq!(
            store.snapshot().unwrap().auth,
            Auth::ApiKey("first-test-key".into())
        );
        assert!(!format!("{:?}", managed.snapshot().unwrap().auth).contains("environment-test-key"));
    }
    #[test]
    fn stale_refresh_cannot_overwrite_logout_or_account_switch() {
        let f = Fixture::new();
        let store = f.store();
        let original = Auth::ChatGpt {
            access: "old-access".into(),
            refresh: "old-refresh".into(),
            account_id: "test-account".into(),
        };
        store.save_unconditionally(&original, "").unwrap();
        let before = store.snapshot().unwrap();
        let updated = Auth::ChatGpt {
            access: "new-access".into(),
            refresh: "new-refresh".into(),
            account_id: "test-account".into(),
        };
        store.logout().unwrap();
        assert!(!store.save_refresh(&before, &updated, "").unwrap());
        assert!(matches!(store.snapshot().unwrap().auth, Auth::Unavailable));
        store
            .save_unconditionally(&Auth::ApiKey("switched-key".into()), "")
            .unwrap();
        assert!(!store.save_refresh(&before, &updated, "").unwrap());
        assert_eq!(
            store.snapshot().unwrap().auth,
            Auth::ApiKey("switched-key".into())
        );
    }
    #[test]
    fn refresh_preserves_generation_and_rejects_parallel_old_rotation() {
        let f = Fixture::new();
        let store = f.store();
        let old = Auth::ChatGpt {
            access: "old".into(),
            refresh: "refresh".into(),
            account_id: "account".into(),
        };
        store.save_unconditionally(&old, "").unwrap();
        let before = store.snapshot().unwrap();
        let updated = Auth::ChatGpt {
            access: "updated".into(),
            refresh: "rotated".into(),
            account_id: "account".into(),
        };
        assert!(store.save_refresh(&before, &updated, "").unwrap());
        assert_eq!(store.snapshot().unwrap().revision, before.revision);
        assert!(!store.save_refresh(&before, &old, "").unwrap());
    }

    #[tokio::test]
    async fn late_refresh_http_result_cannot_restore_logged_out_or_replaced_credentials() {
        for logout in [true, false] {
            let fixture = Fixture::new();
            let store = fixture.store();
            let original = Auth::ChatGpt {
                access: "expired-access".into(),
                refresh: "old-refresh-secret".into(),
                account_id: "test-account".into(),
            };
            store.save_unconditionally(&original, "").unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/oauth/token", listener.local_addr().unwrap());
            let (arrived, request) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![];
                loop {
                    let mut buffer = [0; 1024];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|p| p == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&bytes[..end]);
                        let length = head
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                            .map(|(_, length)| length.trim().parse::<usize>().unwrap())
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let _ = arrived.send(());
                let _ = released.await;
                let body = r#"{"access_token":"late-access-secret","refresh_token":"late-refresh-secret"}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            });
            let worker_store = store.clone();
            let worker = tokio::spawn(async move {
                let mut cached = original;
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap();
                let result = cached
                    .ensure_fresh_at(&client, &worker_store, &endpoint)
                    .await;
                (cached, result.is_err())
            });
            tokio::time::timeout(Duration::from_secs(3), request)
                .await
                .unwrap()
                .unwrap();
            if logout {
                store.logout().unwrap();
            } else {
                store
                    .save_unconditionally(&Auth::ApiKey("replacement-key".into()), "")
                    .unwrap();
            }
            let _ = release.send(());
            let (cached, failed) = tokio::time::timeout(Duration::from_secs(3), worker)
                .await
                .unwrap()
                .unwrap();
            assert!(failed);
            assert_eq!(cached, store.snapshot().unwrap().auth);
            if logout {
                assert!(matches!(cached, Auth::Unavailable));
            } else {
                assert_eq!(cached, Auth::ApiKey("replacement-key".into()));
            }
            server.await.unwrap();
        }
    }
}
