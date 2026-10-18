//! Authentication: API key, or ChatGPT sign-in (OAuth PKCE) with refresh.
//!
//! Observed in a local Codex token: issuer `https://auth.openai.com`, client_id
//! `app_EMoamEEZ73f0CkXaXp7hrann`, scopes `openid profile email offline_access`.
//! Observed in the Codex binary: `/oauth/token`, `chatgpt-account-id`, `originator`, `codex_cli_rs`.

use crate::config::home;
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REDIRECT: &str = "http://localhost:1455/auth/callback";

#[derive(Clone, Debug)]
pub enum Auth {
    ApiKey(String),
    ChatGpt { access: String, refresh: String, account_id: String },
}

fn own_path() -> PathBuf {
    home().join("auth.json")
}
fn codex_path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".codex/auth.json")
}

fn jwt_claims(token: &str) -> Option<Value> {
    let p = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p.trim_end_matches('=')).ok()?).ok()
}

fn parse_file(v: &Value) -> Option<Auth> {
    if let Some(k) = v["OPENAI_API_KEY"].as_str() {
        return Some(Auth::ApiKey(k.into()));
    }
    let t = &v["tokens"];
    Some(Auth::ChatGpt {
        access: t["access_token"].as_str()?.into(),
        refresh: t["refresh_token"].as_str()?.into(),
        account_id: t["account_id"].as_str()?.into(),
    })
}

impl Auth {
    /// Priority: env key > ~/.mcodex/auth.json > ~/.codex/auth.json (re-used read-only).
    pub fn load() -> Result<Self> {
        if let Ok(k) = std::env::var("OPENAI_API_KEY") {
            return Ok(Auth::ApiKey(k));
        }
        for p in [own_path(), codex_path()] {
            if let Some(a) = std::fs::read_to_string(&p).ok().and_then(|s| serde_json::from_str(&s).ok()).and_then(|v| parse_file(&v)) {
                return Ok(a);
            }
        }
        bail!("not signed in: run `mcodex login` or set OPENAI_API_KEY")
    }

    pub fn base_url(&self) -> String {
        match (self, std::env::var("OPENAI_BASE_URL")) {
            (_, Ok(u)) => u,
            (Auth::ApiKey(_), _) => "https://api.openai.com/v1".into(),
            (Auth::ChatGpt { .. }, _) => "https://chatgpt.com/backend-api/codex".into(),
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            Auth::ApiKey(_) => "API key",
            Auth::ChatGpt { .. } => "ChatGPT",
        }
    }

    /// Refresh the ChatGPT access token if it expires within 60s.
    pub async fn ensure_fresh(&mut self, http: &reqwest::Client) -> Result<()> {
        let Auth::ChatGpt { access, refresh, account_id } = self else { return Ok(()) };
        let exp = jwt_claims(access).and_then(|c| c["exp"].as_i64()).unwrap_or(0);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        if exp - now > 60 {
            return Ok(());
        }
        let r: Value = http
            .post(format!("{ISSUER}/oauth/token"))
            .json(&json!({"client_id": CLIENT_ID, "grant_type": "refresh_token", "refresh_token": refresh, "scope": "openid profile email"}))
            .send()
            .await?
            .error_for_status()
            .context("token refresh failed (run `mcodex login`)")?
            .json()
            .await?;
        if let Some(a) = r["access_token"].as_str() {
            *access = a.into();
        }
        if let Some(rt) = r["refresh_token"].as_str() {
            *refresh = rt.into();
        }
        save(access, refresh, r["id_token"].as_str().unwrap_or(""), account_id)?;
        Ok(())
    }
}

fn save(access: &str, refresh: &str, id_token: &str, account_id: &str) -> Result<()> {
    let p = own_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let v = json!({"auth_mode": "chatgpt", "OPENAI_API_KEY": null,
        "tokens": {"id_token": id_token, "access_token": access, "refresh_token": refresh, "account_id": account_id}});
    std::fs::write(&p, serde_json::to_string_pretty(&v)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn urlenc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
fn urldec(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into()
}

fn rand_b64(n: usize) -> String {
    let mut b = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// Browser-based OAuth PKCE sign-in with a localhost callback on :1455.
pub async fn login() -> Result<()> {
    let verifier = rand_b64(48);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = rand_b64(16);
    let url = format!(
        "{ISSUER}/oauth/authorize?response_type=code&client_id={CLIENT_ID}&redirect_uri={}&scope={}&code_challenge={challenge}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true&state={state}&originator=codex_cli_rs",
        urlenc(REDIRECT),
        urlenc("openid profile email offline_access")
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:1455").await.context("port 1455 busy")?;
    println!("Opening browser to sign in:\n{url}");
    let _ = tokio::process::Command::new("open").arg(&url).status().await;

    let code = loop {
        let (mut sock, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let target = req.lines().next().and_then(|l| l.split(' ').nth(1)).unwrap_or("");
        if !target.starts_with("/auth/callback") {
            sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n").await?;
            continue;
        }
        let q = target.split_once('?').map(|x| x.1).unwrap_or("");
        let get = |k: &str| q.split('&').find_map(|p| p.strip_prefix(&format!("{k}="))).map(urldec);
        let ok = get("state").as_deref() == Some(state.as_str()) && get("code").is_some();
        let body = if ok { "Signed in. You can close this tab." } else { "Sign-in failed." };
        sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
        if ok {
            break get("code").unwrap();
        }
        bail!("state mismatch or missing code");
    };

    let http = reqwest::Client::new();
    let r: Value = http
        .post(format!("{ISSUER}/oauth/token"))
        .form(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT), ("client_id", CLIENT_ID), ("code_verifier", &verifier)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let id = r["id_token"].as_str().ok_or_else(|| anyhow!("no id_token"))?;
    let account = jwt_claims(id)
        .and_then(|c| c["https://api.openai.com/auth"]["chatgpt_account_id"].as_str().map(String::from))
        .ok_or_else(|| anyhow!("no chatgpt_account_id in id_token"))?;
    save(
        r["access_token"].as_str().ok_or_else(|| anyhow!("no access_token"))?,
        r["refresh_token"].as_str().ok_or_else(|| anyhow!("no refresh_token"))?,
        id,
        &account,
    )?;
    println!("Signed in. Credentials saved to {}", own_path().display());
    Ok(())
}

pub fn login_with_api_key(key: &str) -> Result<()> {
    let p = own_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    std::fs::write(&p, serde_json::to_string_pretty(&json!({"auth_mode": "apikey", "OPENAI_API_KEY": key}))?)?;
    Ok(())
}

pub fn logout() -> Result<bool> {
    Ok(std::fs::remove_file(own_path()).is_ok())
}
