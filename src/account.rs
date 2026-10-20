//! Desktop account RPCs. Public responses never include credential material.

use crate::auth::{self, Auth, OAuthAttempt, Store};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tokio::task::JoinHandle;

struct Login {
    id: String,
    status: &'static str,
    auth_url: Option<String>,
    error: Option<String>,
    cancel: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Login {
    fn public(&self) -> Value {
        let mut value = json!({"id":self.id,"status":self.status});
        if let Some(url) = &self.auth_url {
            value["authUrl"] = json!(url);
        }
        if let Some(error) = &self.error {
            value["error"] = json!(error);
        }
        value
    }
}

#[derive(Default)]
struct State {
    generation: u64,
    login: Option<Login>,
}

struct Options {
    issuer: String,
    api_base: String,
    callback_port: u16,
    lifetime: Duration,
}

pub struct Account {
    store: Store,
    state: Arc<Mutex<State>>,
    http: reqwest::Client,
    options: Options,
    out: UnboundedSender<Value>,
}

impl Account {
    pub fn new(out: UnboundedSender<Value>) -> Self {
        Self::with_config(
            Store::configured(),
            out,
            Options {
                issuer: auth::ISSUER.into(),
                api_base: std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
                callback_port: 1455,
                lifetime: Duration::from_secs(300),
            },
            reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static account HTTP client configuration"),
        )
    }

    fn with_config(
        store: Store,
        out: UnboundedSender<Value>,
        options: Options,
        http: reqwest::Client,
    ) -> Self {
        Self {
            store,
            state: Arc::new(Mutex::new(State::default())),
            http,
            options,
            out,
        }
    }

    fn read(&self) -> Result<Value, String> {
        public_metadata(&self.store, &self.state)
    }

    pub async fn handle(&self, method: &str, params: &Value) -> Option<Result<Value, String>> {
        if !matches!(
            method,
            "account/read" | "account/login/start" | "account/login/cancel" | "account/logout"
        ) {
            return None;
        }
        Some(self.dispatch(method, params).await)
    }

    async fn dispatch(&self, method: &str, params: &Value) -> Result<Value, String> {
        if !params.is_object() && !params.is_null() {
            return Err("Account parameters must be an object".into());
        }
        match method {
            "account/read" => self.read(),
            "account/login/start" => {
                self.store.ensure_mutable().map_err(public_error)?;
                match params["type"].as_str() {
                    Some("apiKey") => {
                        let key = params["apiKey"].as_str().ok_or("apiKey must be a string")?;
                        let key = auth::validate_key(key).map_err(public_error)?.to_string();
                        let generation = self.invalidate(None).await?.0;
                        let revision = self.store.snapshot().map_err(public_error)?.revision;
                        self.validate_api_key(&key).await?;
                        {
                            let state = self
                                .state
                                .lock()
                                .map_err(|_| "Account lock is unavailable")?;
                            if state.generation != generation {
                                return Err("Sign-in was cancelled or replaced; existing credentials were kept".into());
                            }
                            self.store
                                .save_if_revision(&revision, &Auth::ApiKey(key), "")
                                .map_err(public_error)?;
                        }
                        let mut result = self.read()?;
                        result["type"] = json!("apiKey");
                        emit(&self.out, "account/updated", self.read()?);
                        Ok(result)
                    }
                    Some("chatgpt") => self.start_oauth().await,
                    _ => Err("Login type must be apiKey or chatgpt".into()),
                }
            }
            "account/login/cancel" => {
                let id = params["loginId"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or("loginId is required")?;
                let (_, cancelled) = self.invalidate(Some(id)).await?;
                Ok(json!({"cancelled":cancelled}))
            }
            "account/logout" => {
                self.store.ensure_mutable().map_err(public_error)?;
                self.invalidate(None).await?;
                self.store.logout().map_err(public_error)?;
                let result = self.read()?;
                emit(&self.out, "account/updated", result.clone());
                Ok(result)
            }
            _ => unreachable!(),
        }
    }

    async fn validate_api_key(&self, key: &str) -> Result<(), String> {
        let endpoint = format!("{}/models", self.options.api_base.trim_end_matches('/'));
        let url = reqwest::Url::parse(&endpoint).map_err(|_| "Configured API URL is invalid")?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Configured API URL must use HTTP(S) without embedded credentials, query, or fragment".into());
        }
        let response = self.http.get(url).bearer_auth(key).send().await.map_err(|_| "Could not validate the API key. Check the API endpoint and connection; existing credentials were kept.")?;
        if !response.status().is_success() {
            return Err(format!(
                "API key validation failed (HTTP {}). Existing credentials were kept.",
                response.status().as_u16()
            ));
        }
        let body: Value = response.json().await.map_err(|_| {
            "The API endpoint returned an invalid models response; existing credentials were kept"
        })?;
        if !body["data"].is_array() {
            return Err(
                "The API endpoint did not return a models list; existing credentials were kept"
                    .into(),
            );
        }
        Ok(())
    }

    async fn start_oauth(&self) -> Result<Value, String> {
        let generation = self.invalidate(None).await?.0;
        let revision = self.store.snapshot().map_err(public_error)?.revision;
        let attempt = OAuthAttempt::start(&self.options.issuer, self.options.callback_port)
            .await
            .map_err(public_error)?;
        let id = auth::rand_b64(24);
        let auth_url = attempt.auth_url.clone();
        let (cancel, cancelled) = oneshot::channel();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Account lock is unavailable")?;
            if state.generation != generation {
                return Err("Sign-in was cancelled or replaced".into());
            }
            state.login = Some(Login {
                id: id.clone(),
                status: "pending",
                auth_url: Some(auth_url.clone()),
                error: None,
                cancel: Some(cancel),
                task: None,
            });
        }
        let (state, store, http, out, login_id, lifetime) = (
            self.state.clone(),
            self.store.clone(),
            self.http.clone(),
            self.out.clone(),
            id.clone(),
            self.options.lifetime,
        );
        let task = tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancelled => Err("Sign-in cancelled; existing credentials were kept".to_string()),
                result = tokio::time::timeout(lifetime, attempt.finish(&http)) => match result {
                    Ok(result) => result.map_err(public_error),
                    Err(_) => Err("Sign-in timed out; existing credentials were kept".into()),
                },
            };
            let completed = {
                let Ok(mut current) = state.lock() else {
                    return;
                };
                if current.generation != generation
                    || current
                        .login
                        .as_ref()
                        .is_none_or(|login| login.id != login_id || login.status != "pending")
                {
                    return;
                }
                let result = result.and_then(|(auth, token)| {
                    store
                        .save_if_revision(&revision, &auth, &token)
                        .map_err(public_error)
                });
                let login = current.login.as_mut().unwrap();
                login.cancel = None;
                login.auth_url = None;
                match result {
                    Ok(()) => {
                        login.status = "succeeded";
                        json!({"loginId":login_id,"success":true})
                    }
                    Err(error) => {
                        login.status = "failed";
                        login.error = Some(error.clone());
                        json!({"loginId":login_id,"success":false,"error":error})
                    }
                }
            };
            if completed["success"] == true {
                if let Ok(value) = public_metadata(&store, &state) {
                    emit(&out, "account/updated", value);
                }
            }
            emit(&out, "account/login/completed", completed);
        });
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Account lock is unavailable")?;
            if state.generation == generation {
                if let Some(login) = state.login.as_mut() {
                    login.task = Some(task);
                }
            } else {
                task.abort();
            }
        }
        Ok(json!({"loginId":id,"authUrl":auth_url,"type":"chatgpt"}))
    }

    /// Invalidate before awaiting shutdown. A late callback/HTTP response cannot
    /// commit after Cancel, Logout, or another sign-in starts.
    async fn invalidate(&self, requested: Option<&str>) -> Result<(u64, bool), String> {
        let (generation, task, notification, cancelled) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "Account lock is unavailable")?;
            if let Some(id) = requested {
                if state.login.as_ref().is_none_or(|login| login.id != id) {
                    return Err("No matching sign-in exists".into());
                }
                if state
                    .login
                    .as_ref()
                    .is_some_and(|login| login.status != "pending")
                {
                    return Ok((state.generation, false));
                }
            }
            state.generation = state.generation.wrapping_add(1);
            let mut task = None;
            let mut notification = None;
            let mut cancelled = false;
            if let Some(login) = state.login.as_mut() {
                if login.status == "pending" {
                    login.status = "cancelled";
                    login.auth_url = None;
                    login.error = None;
                    cancelled = true;
                    if let Some(cancel) = login.cancel.take() {
                        let _ = cancel.send(());
                    }
                    notification = Some(
                        json!({"loginId":login.id,"success":false,"cancelled":true,"error":"Sign-in cancelled"}),
                    );
                }
                task = login.task.take();
            }
            (state.generation, task, notification, cancelled)
        };
        if let Some(task) = task {
            task.abort();
            let _ = task.await;
        }
        if let Some(value) = notification {
            emit(&self.out, "account/login/completed", value);
        }
        Ok((generation, cancelled))
    }

    pub async fn shutdown(&self) {
        let _ = self.invalidate(None).await;
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            state.generation = state.generation.wrapping_add(1);
            if let Some(login) = state.login.as_mut() {
                if let Some(cancel) = login.cancel.take() {
                    let _ = cancel.send(());
                }
                if let Some(task) = login.task.take() {
                    task.abort();
                }
            }
        }
    }
}

fn public_error(error: anyhow::Error) -> String {
    format!("{error:#}")
}
fn emit(out: &UnboundedSender<Value>, method: &str, params: Value) {
    let _ = out.send(json!({"method":method,"params":params}));
}
fn public_metadata(store: &Store, state: &Mutex<State>) -> Result<Value, String> {
    let mut value = store.metadata().map_err(public_error)?;
    value["login"] = state
        .lock()
        .map_err(|_| "Account lock is unavailable")?
        .login
        .as_ref()
        .map(Login::public)
        .unwrap_or(Value::Null);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!("mcodex-account-{}", auth::rand_b64(18))))
        }
        fn store(&self) -> Store {
            Store::isolated(self.0.clone(), None, None)
        }
        fn account(
            &self,
            base: &str,
            lifetime: Duration,
        ) -> (Account, tokio::sync::mpsc::UnboundedReceiver<Value>) {
            let (out, rx) = tokio::sync::mpsc::unbounded_channel();
            let http = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            (
                Account::with_config(
                    self.store(),
                    out,
                    Options {
                        issuer: base.into(),
                        api_base: base.into(),
                        callback_port: 0,
                        lifetime,
                    },
                    http,
                ),
                rx,
            )
        }
        fn save_key(&self, key: &str) {
            let store = self.store();
            let revision = store.snapshot().unwrap().revision;
            store
                .save_if_revision(&revision, &Auth::ApiKey(key.into()), "")
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn call(account: &Account, method: &str, p: Value) -> Result<Value, String> {
        account.handle(method, &p).await.unwrap()
    }
    async fn mock_http(
        status: u16,
        body: Value,
        gate: Option<oneshot::Receiver<()>>,
    ) -> (String, oneshot::Receiver<String>, JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|p| p == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]);
                    let length = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                        .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let _ = tx.send(String::from_utf8(request).unwrap());
            if let Some(gate) = gate {
                let _ = tokio::time::timeout(Duration::from_secs(3), gate).await;
            }
            let body = serde_json::to_string(&body).unwrap();
            let _=socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
        });
        (base, rx, task)
    }
    fn token_response() -> Value {
        let claims = json!({"email":"fixture@example.invalid","https://api.openai.com/auth":{"chatgpt_account_id":"fixture-account","chatgpt_plan_type":"test"}});
        let token = format!(
            "e30.{}.signature",
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(serde_json::to_vec(&claims).unwrap())
        );
        json!({"access_token":"fixture-access-secret","refresh_token":"fixture-refresh-secret","id_token":token})
    }
    async fn authorize(
        account: &Account,
        start: &Value,
        state_override: Option<&str>,
    ) -> reqwest::Response {
        let url = reqwest::Url::parse(start["authUrl"].as_str().unwrap()).unwrap();
        let pairs: std::collections::HashMap<_, _> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let mut callback = reqwest::Url::parse(&pairs["redirect_uri"]).unwrap();
        callback
            .query_pairs_mut()
            .append_pair("state", state_override.unwrap_or(&pairs["state"]))
            .append_pair("code", "fixture-code");
        account.http.get(callback).send().await.unwrap()
    }
    async fn wait_login(account: &Account, status: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let v = account.read().unwrap();
                if v["login"]["status"] == status {
                    return v;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn api_key_validation_saves_private_credentials_and_logout_persists() {
        let f = Fixture::new();
        f.save_key("previous-test-key");
        let (base, request, server) = mock_http(200, json!({"data":[]}), None).await;
        let (account, _) = f.account(&base, Duration::from_secs(2));
        let result = call(
            &account,
            "account/login/start",
            json!({"type":"apiKey","apiKey":"new-test-key"}),
        )
        .await
        .unwrap();
        assert_eq!(result["account"]["type"], "apiKey");
        assert!(request.await.unwrap().contains("Bearer new-test-key"));
        server.await.unwrap();
        assert_eq!(
            f.store().snapshot().unwrap().auth,
            Auth::ApiKey("new-test-key".into())
        );
        assert!(!result.to_string().contains("new-test-key"));
        let result = call(&account, "account/logout", json!({})).await.unwrap();
        assert!(result["account"].is_null());
        assert!(matches!(
            f.store().snapshot().unwrap().auth,
            Auth::Unavailable
        ));
        account.shutdown().await;
    }
    #[tokio::test]
    async fn failed_key_validation_never_saves_or_reflects_provider_error_bodies() {
        let f = Fixture::new();
        f.save_key("keep-test-key");
        let (base, _, server) = mock_http(
            401,
            json!({"error":"new-secret echoed by untrusted endpoint"}),
            None,
        )
        .await;
        let (account, _) = f.account(&base, Duration::from_secs(2));
        let error = call(
            &account,
            "account/login/start",
            json!({"type":"apiKey","apiKey":"new-secret"}),
        )
        .await
        .unwrap_err();
        assert!(error.contains("401"));
        assert!(!error.contains("new-secret"));
        assert!(!error.contains("untrusted"));
        assert_eq!(
            f.store().snapshot().unwrap().auth,
            Auth::ApiKey("keep-test-key".into())
        );
        server.await.unwrap();
        account.shutdown().await;
    }
    #[tokio::test]
    async fn oauth_pkce_callback_commits_and_emits_only_public_metadata() {
        let f = Fixture::new();
        let (base, request, server) = mock_http(200, token_response(), None).await;
        let (account, mut events) = f.account(&base, Duration::from_secs(2));
        let start = call(&account, "account/login/start", json!({"type":"chatgpt"}))
            .await
            .unwrap();
        assert_eq!(
            authorize(&account, &start, Some("wrong-state"))
                .await
                .status()
                .as_u16(),
            400
        );
        assert_eq!(account.read().unwrap()["login"]["status"], "pending");
        assert_eq!(
            authorize(&account, &start, None).await.status().as_u16(),
            200
        );
        let result = wait_login(&account, "succeeded").await;
        assert_eq!(result["account"]["email"], "fixture@example.invalid");
        assert_eq!(result["credentialSource"], "local");
        let request = request.await.unwrap();
        assert!(request.contains("code_verifier="));
        let auth_url = reqwest::Url::parse(start["authUrl"].as_str().unwrap()).unwrap();
        let challenge = auth_url
            .query_pairs()
            .find(|(k, _)| k == "code_challenge")
            .unwrap()
            .1
            .into_owned();
        assert!(!request.contains(&challenge));
        server.await.unwrap();
        let mut completed = false;
        while let Ok(event) = events.try_recv() {
            let text = event.to_string();
            assert!(!text.contains("fixture-access-secret"));
            assert!(!text.contains("fixture-refresh-secret"));
            if event["method"] == "account/login/completed" {
                assert_eq!(event["params"]["success"], true);
                completed = true;
            }
        }
        assert!(completed);
        account.shutdown().await;
    }
    #[tokio::test]
    async fn cancellation_and_timeout_leave_previous_credentials_unchanged() {
        let f = Fixture::new();
        f.save_key("keep-key");
        let (account, mut events) = f.account("http://127.0.0.1:1", Duration::from_millis(80));
        let before = std::fs::read(f.0.join("auth.json")).unwrap();
        let start = call(&account, "account/login/start", json!({"type":"chatgpt"}))
            .await
            .unwrap();
        call(
            &account,
            "account/login/cancel",
            json!({"loginId":start["loginId"]}),
        )
        .await
        .unwrap();
        assert_eq!(account.read().unwrap()["login"]["status"], "cancelled");
        let notification = events.try_recv().unwrap();
        assert_eq!(notification["method"], "account/login/completed");
        assert_eq!(notification["params"]["success"], false);
        assert_eq!(notification["params"]["cancelled"], true);
        assert_eq!(std::fs::read(f.0.join("auth.json")).unwrap(), before);
        call(&account, "account/login/start", json!({"type":"chatgpt"}))
            .await
            .unwrap();
        let result = wait_login(&account, "failed").await;
        assert!(result["login"]["error"]
            .as_str()
            .unwrap()
            .contains("timed out"));
        assert_eq!(std::fs::read(f.0.join("auth.json")).unwrap(), before);
        account.shutdown().await;
    }
    #[tokio::test]
    async fn logout_during_oauth_exchange_discards_late_success() {
        let f = Fixture::new();
        f.save_key("old-key");
        let (release, gate) = oneshot::channel();
        let (base, request, server) = mock_http(200, token_response(), Some(gate)).await;
        let (account, _) = f.account(&base, Duration::from_secs(2));
        let start = call(&account, "account/login/start", json!({"type":"chatgpt"}))
            .await
            .unwrap();
        authorize(&account, &start, None).await;
        request.await.unwrap();
        call(&account, "account/logout", json!({})).await.unwrap();
        let _ = release.send(());
        server.await.unwrap();
        assert!(account.read().unwrap()["account"].is_null());
        assert!(matches!(
            f.store().snapshot().unwrap().auth,
            Auth::Unavailable
        ));
        account.shutdown().await;
    }
    #[tokio::test]
    async fn environment_credentials_are_read_only_and_public() {
        let f = Fixture::new();
        let (out, _) = tokio::sync::mpsc::unbounded_channel();
        let account = Account::with_config(
            Store::isolated(f.0.clone(), None, Some("managed-test-key".into())),
            out,
            Options {
                issuer: "http://127.0.0.1:1".into(),
                api_base: "http://127.0.0.1:1".into(),
                callback_port: 0,
                lifetime: Duration::from_secs(1),
            },
            reqwest::Client::new(),
        );
        let read = account.read().unwrap();
        assert_eq!(read["credentialSource"], "environment");
        assert_eq!(read["canChangeCredentials"], false);
        assert!(!read.to_string().contains("managed-test-key"));
        for (method, p) in [
            ("account/login/start", json!({"type":"chatgpt"})),
            (
                "account/login/start",
                json!({"type":"apiKey","apiKey":"new-key"}),
            ),
            ("account/logout", json!({})),
        ] {
            assert!(call(&account, method, p)
                .await
                .unwrap_err()
                .contains("OPENAI_API_KEY"));
        }
        assert!(!f.0.exists());
        account.shutdown().await;
    }

    #[tokio::test]
    async fn logout_while_validating_an_api_key_discards_the_late_valid_key() {
        let fixture = Fixture::new();
        fixture.save_key("previous-key");
        let (release, gate) = oneshot::channel();
        let (base, request, server) = mock_http(200, json!({"data":[]}), Some(gate)).await;
        let (account, _) = fixture.account(&base, Duration::from_secs(2));
        let account = Arc::new(account);
        let signer = account.clone();
        let worker = tokio::spawn(async move {
            call(
                &signer,
                "account/login/start",
                json!({"type":"apiKey","apiKey":"late-key"}),
            )
            .await
        });
        request.await.unwrap();
        call(&account, "account/logout", json!({})).await.unwrap();
        let _ = release.send(());
        assert!(worker.await.unwrap().is_err());
        server.await.unwrap();
        assert!(matches!(
            fixture.store().snapshot().unwrap().auth,
            Auth::Unavailable
        ));
        account.shutdown().await;
    }
}
