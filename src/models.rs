//! Live provider catalog. No model name is treated as proof of a capability.
use crate::auth::{Auth, Store};
use futures_util::StreamExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

pub const DEFAULT_MODEL: &str = "gpt-6.1-sol";
pub const CODEX_CLIENT_VERSION: &str = "0.163.0";
const BUNDLED_CODEX_MODELS: &str = include_str!("vendor/codex-models.json");

const MAX_BODY: usize = 4 * 1024 * 1024;
pub const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

struct Cache {
    identity: String,
    models: Vec<Value>,
    fetched_at: u64,
    checked: Instant,
    error: Option<String>,
}
#[derive(Default)]
pub struct Catalog {
    cache: Mutex<Option<Cache>>,
}

fn text(v: &Value, max: usize) -> Option<&str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control))
}
fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn identity(auth: &Auth, base: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(base.as_bytes());
    hash.update([0]);
    match auth {
        Auth::Unavailable => hash.update(b"signed-out"),
        Auth::ApiKey(key) => {
            hash.update(b"api-key\0");
            hash.update(key.as_bytes());
        }
        Auth::ChatGpt {
            access, account_id, ..
        } => {
            hash.update(b"chatgpt\0");
            hash.update(access.as_bytes());
            hash.update([0]);
            hash.update(account_id.as_bytes());
        }
    }
    format!("{:x}", hash.finalize())
}

fn parse_catalog(value: &Value, chatgpt: bool) -> Result<Vec<Value>, String> {
    let rows = value[if chatgpt { "models" } else { "data" }]
        .as_array()
        .ok_or("The provider returned an invalid model catalog")?;
    if rows.len() > 4096 {
        return Err("The provider model catalog is too large".into());
    }
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in rows {
        let id = text(&row[if chatgpt { "slug" } else { "id" }], 255)
            .ok_or("The provider returned an invalid model ID")?;
        if !seen.insert(id) {
            continue;
        }
        let mut efforts = Vec::new();
        // Only the Codex catalog defines this contract. The standard Models API does not.
        if chatgpt {
            if let Some(levels) = row["supported_reasoning_levels"].as_array() {
                if levels.len() > 32 {
                    return Err("The provider returned invalid reasoning metadata".into());
                }
                for level in levels {
                    if let Some(effort) = text(&level["effort"], 32).filter(|e| EFFORTS.contains(e))
                    {
                        if !efforts
                            .iter()
                            .any(|v: &Value| v["reasoningEffort"] == effort)
                        {
                            efforts.push(json!({"reasoningEffort":effort,"label":effort,"description":text(&level["description"],1024).unwrap_or("")}));
                        }
                    }
                }
            }
        }
        let default = text(&row["default_reasoning_level"], 32)
            .filter(|e| efforts.iter().any(|v| v["reasoningEffort"] == *e));
        let tiers: Vec<Value> = if chatgpt {
            row["service_tiers"].as_array().map(|a|a.iter().take(16).filter_map(|v|{
            let id=text(&v["id"],32)?;Some(json!({"id":id,"name":text(&v["name"],80).unwrap_or(id),"description":text(&v["description"],512).unwrap_or("")}))
        }).collect()).unwrap_or_default()
        } else {
            Vec::new()
        };
        models.push(json!({"id":id,"model":id,"displayName":if chatgpt {text(&row["display_name"],255).unwrap_or(id)}else{id},
            "description":if chatgpt{text(&row["description"],2048).unwrap_or("")}else{"Available to this account. Responses and reasoning capabilities are not reported by this catalog."},
            "hidden":chatgpt&&row["visibility"]!="list","priority":row["priority"].as_i64().unwrap_or(1000),"upgrade":row["upgrade"].clone(),"isDefault":false,"source":"provider","available":true,
            "defaultReasoningEffort":default.unwrap_or("auto"),"supportedReasoningEfforts":efforts,
            "reasoningMetadataKnown":chatgpt&&row["supported_reasoning_levels"].is_array(),
            "responsesSupport":if chatgpt{"supported"}else{"unknown"},"inputModalities":if chatgpt{row["input_modalities"].clone()}else{Value::Null},
            "defaultServiceTier":row["default_service_tier"],"serviceTiers":tiers,"contextWindow":if chatgpt{row["context_window"].as_u64()}else{None},"supportedServiceTiers":tiers,
            "shutdownDate":if chatgpt{Value::Null}else{row["shutdown_date"].clone()}}));
    }
    Ok(models)
}

async fn fetch(http: &reqwest::Client, auth: &Auth, base: &str) -> Result<Vec<Value>, String> {
    let chatgpt = matches!(auth, Auth::ChatGpt { .. });
    let mut url = reqwest::Url::parse(&format!("{}/models", base.trim_end_matches('/')))
        .map_err(|_| "The model provider URL is invalid")?;
    if chatgpt {
        url.query_pairs_mut()
            .append_pair("client_version", CODEX_CLIENT_VERSION);
    }
    let mut request = http
        .get(url)
        .header("originator", "codex_cli_rs")
        .header("accept", "application/json");
    request = match auth {
        Auth::ApiKey(key) => request.bearer_auth(key),
        Auth::ChatGpt {
            access, account_id, ..
        } => request
            .bearer_auth(access)
            .header("chatgpt-account-id", account_id),
        Auth::Unavailable => return Err("Sign in to refresh available models".into()),
    };
    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "The model catalog request timed out"
        } else {
            "Cannot reach the model provider"
        }
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "Model catalog request failed (HTTP {})",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err("The provider model catalog is too large".into());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "The model catalog download failed")?;
        if body.len() + chunk.len() > MAX_BODY {
            return Err("The provider model catalog is too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    let value = serde_json::from_slice(&body)
        .map_err(|_| "The provider returned an invalid model catalog")?;
    parse_catalog(&value, chatgpt)
}

fn codex_provider(base: &str) -> bool {
    reqwest::Url::parse(base)
        .is_ok_and(|url| matches!(url.host_str(), Some("api.openai.com" | "chatgpt.com")))
}
fn codex_fallback() -> (Vec<Value>, &'static str) {
    if crate::config::codex_import_enabled() {
        let path = crate::config::codex_home().join("models_cache.json");
        if std::fs::metadata(&path).is_ok_and(|meta| meta.len() <= MAX_BODY as u64) {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    if let Ok(models) = parse_catalog(&value, true) {
                        if !models.is_empty() {
                            return (models, "codex-cache");
                        }
                    }
                }
            }
        }
    }
    (
        parse_catalog(
            &serde_json::from_str::<Value>(BUNDLED_CODEX_MODELS).unwrap(),
            true,
        )
        .unwrap(),
        "codex-bundled",
    )
}
pub fn default_effort(model: &str) -> String {
    codex_fallback()
        .0
        .iter()
        .find(|item| item["model"] == model)
        .and_then(|item| item["defaultReasoningEffort"].as_str())
        .unwrap_or("auto")
        .to_owned()
}
pub fn resolve_effort(model: &Value, requested: &str) -> String {
    if model["reasoningMetadataKnown"] == true
        && model["supportedReasoningEfforts"]
            .as_array()
            .is_some_and(|options| !options.is_empty())
    {
        if model["supportedReasoningEfforts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| option["reasoningEffort"] == requested)
        {
            return requested.into();
        }
        return model["defaultReasoningEffort"]
            .as_str()
            .unwrap_or("auto")
            .into();
    }
    requested.into()
}
pub fn normalize_tier(tier: &str) -> Option<String> {
    match tier {
        "default" | "auto" | "" => None,
        "fast" => Some("priority".into()),
        other => Some(other.into()),
    }
}

impl Catalog {
    pub async fn metadata(&self, model: &str) -> Option<Value> {
        let auth = Store::configured().snapshot().ok()?.auth;
        let base = auth.base_url();
        let cache = self.cache.lock().await;
        if let Some(metadata) = cache
            .as_ref()
            .filter(|cache| cache.identity == identity(&auth, &base))
            .and_then(|cache| cache.models.iter().find(|item| item["model"] == model))
        {
            return Some(metadata.clone());
        }
        if codex_provider(&base) {
            return codex_fallback()
                .0
                .into_iter()
                .find(|item| item["model"] == model);
        }
        None
    }
    pub async fn invalidate(&self) {
        *self.cache.lock().await = None;
    }
    pub async fn list(
        &self,
        p: &Value,
        configured: Vec<String>,
        default_model: &str,
        effort: &str,
        tier: &str,
    ) -> Result<Value, String> {
        if p.get("refresh").is_some_and(|v| !v.is_boolean()) {
            return Err("refresh must be boolean".into());
        }
        if p.get("includeHidden").is_some_and(|value|!value.is_boolean()){return Err("includeHidden must be boolean".into());}
        let auth = Store::configured()
            .snapshot()
            .map_err(|_| "Cannot read account connection")?
            .auth;
        tokio::time::timeout(
            Duration::from_secs(15),
            self.list_with(
                auth,
                p["refresh"] == true,
                configured,
                default_model,
                effort,
                tier,
                true,
            ),
        )
        .await
        .map_err(|_| "The model catalog is busy. Try refreshing again.".to_string())?
        .map(|mut result|{if p["includeHidden"]!=true{if let Some(items)=result["data"].as_array_mut(){items.retain(|item|item["hidden"]!=true);}}result})
    }
    async fn list_with(
        &self,
        mut auth: Auth,
        refresh: bool,
        configured: Vec<String>,
        default_model: &str,
        effort: &str,
        tier: &str,
        check_account: bool,
    ) -> Result<Value, String> {
        let mut cache = self.cache.lock().await;
        // A request can wait behind another refresh while logout or key replacement completes.
        // Never begin a queued refresh with the credentials captured before that wait.
        if check_account {
            auth = Store::configured()
                .snapshot()
                .map_err(|_| "Cannot read account connection")?
                .auth;
        }
        let base = auth.base_url();
        let key = identity(&auth, &base);
        if cache.as_ref().is_some_and(|c| c.identity != key) {
            *cache = None;
        }
        if !matches!(auth, Auth::Unavailable)
            && !(matches!(auth, Auth::ApiKey(_)) && codex_provider(&base))
            && (refresh
                || cache.as_ref().is_none_or(|c| {
                    c.checked.elapsed()
                        >= Duration::from_secs(if c.error.is_some() { 30 } else { 300 })
                }))
        {
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(12))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Cannot prepare the model connection")?;
            let result = tokio::time::timeout(Duration::from_secs(12), async {
                auth.ensure_fresh(&http)
                    .await
                    .map_err(|_| "Reconnect your account to refresh models".to_string())?;
                if check_account
                    && Store::configured()
                        .snapshot()
                        .map_err(|_| "Cannot read account connection")?
                        .auth
                        != auth
                {
                    return Err(
                        "The account changed before models were requested. Refresh the catalog."
                            .into(),
                    );
                }
                fetch(&http, &auth, &base).await
            })
            .await
            .unwrap_or_else(|_| Err("The model catalog request timed out".into()));
            if check_account
                && Store::configured()
                    .snapshot()
                    .map_err(|_| "Cannot read account connection")?
                    .auth
                    != auth
            {
                *cache = None;
                return Err(
                    "The account changed while models were loading. Refresh the catalog.".into(),
                );
            }
            let refreshed_key = identity(&auth, &base);
            match result {
                Ok(models) => {
                    *cache = Some(Cache {
                        identity: refreshed_key,
                        models,
                        fetched_at: timestamp(),
                        checked: Instant::now(),
                        error: None,
                    })
                }
                Err(error) => {
                    if let Some(c) = cache.as_mut() {
                        c.error = Some(error);
                        c.checked = Instant::now();
                    } else {
                        *cache = Some(Cache {
                            identity: refreshed_key,
                            models: vec![],
                            fetched_at: 0,
                            checked: Instant::now(),
                            error: Some(error),
                        });
                    }
                }
            }
        }
        let mut source = "provider";
        let mut data = cache.as_ref().map(|c| c.models.clone()).unwrap_or_default();
        if data.is_empty() && codex_provider(&base) {
            let fallback = codex_fallback();
            data = fallback.0;
            source = fallback.1;
            for model in &mut data {
                model["source"] = json!(source);
                model["available"] = Value::Null;
            }
        }

        for id in std::iter::once(default_model.to_owned()).chain(configured) {
            if id.is_empty() || id.len() > 255 || id.chars().any(char::is_control) {
                continue;
            }
            if let Some(model) = data.iter_mut().find(|m| m["id"] == id) {
                model["configured"] = json!(true);
            } else {
                data.push(json!({"id":id,"model":id,"displayName":id,"description":"Configured model. Availability and capabilities have not been reported by the provider.","hidden":false,"isDefault":false,"source":"configured","configured":true,"available":Value::Null,"defaultReasoningEffort":"auto","supportedReasoningEfforts":[],"reasoningMetadataKnown":false,"responsesSupport":"unknown","supportedServiceTiers":[]}));
            }
        }
        for model in &mut data {
            model["isDefault"] = json!(model["id"] == default_model);
        }
        data.sort_by(|a, b| {
            a["priority"]
                .as_i64()
                .unwrap_or(1000)
                .cmp(&b["priority"].as_i64().unwrap_or(1000))
        });
        let selected = data.iter().find(|model| model["model"] == default_model);
        let effective_effort = selected
            .map(|model| resolve_effort(model, effort))
            .unwrap_or_else(|| effort.into());
        let c = cache.as_ref();
        let available = c.is_some_and(|c| c.fetched_at > 0);
        Ok(
            json!({"data":data,"defaultModel":default_model,"defaultReasoningEffort":effective_effort,"serviceTier":normalize_tier(tier),"nextCursor":null,
            "catalog":{"source":if available{"provider"}else if source!="provider"{source}else{"configured"},"available":available,"stale":available&&c.is_some_and(|c|c.error.is_some()),"fetchedAt":c.map(|c|c.fetched_at).filter(|t|*t>0),"error":c.and_then(|c|c.error.as_deref()).or_else(||matches!(auth,Auth::Unavailable).then_some("Sign in to refresh available models")),"clientVersion":CODEX_CLIENT_VERSION,"reasoningMetadataAvailable":matches!(auth,Auth::ChatGpt{..})||source!="provider"}}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_catalog_does_not_invent_reasoning_or_responses_support() {
        let data = parse_catalog(
            &json!({"data":[{"id":"text-embedding-3-small"},{"id":"custom"}]}),
            false,
        )
        .unwrap();
        assert_eq!(data[0]["supportedReasoningEfforts"], json!([]));
        assert_eq!(data[1]["responsesSupport"], "unknown");
        assert_eq!(data[1]["defaultReasoningEffort"], "auto");
    }
    #[test]
    fn codex_catalog_retains_only_reported_metadata() {
        let data=parse_catalog(&json!({"models":[{"slug":"example","display_name":"Example","visibility":"list","default_reasoning_level":"high","supported_reasoning_levels":[{"effort":"low"},{"effort":"high"},{"effort":"invented"}],"service_tiers":[{"id":"priority","name":"Fast"}],"context_window":123456}]}),true).unwrap();
        assert_eq!(
            data[0]["supportedReasoningEfforts"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(data[0]["defaultReasoningEffort"], "high");
        assert_eq!(data[0]["contextWindow"], 123456);
        assert_eq!(data[0]["supportedServiceTiers"][0]["id"], "priority");
        assert!(parse_catalog(&json!({"models":[{"slug":"bad\nvalue"}]}), true).is_err());
    }
    #[tokio::test]
    async fn signed_out_catalog_preserves_configured_model_without_network() {
        let c = Catalog::default();
        let value = c
            .list_with(
                Auth::Unavailable,
                false,
                vec!["custom".into()],
                "selected",
                "medium",
                "auto",
                false,
            )
            .await
            .unwrap();
        assert_eq!(value["defaultModel"], "selected");
        assert!(value["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["model"] == "custom"));
        assert_eq!(value["catalog"]["available"], false);
        assert!(value["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == "selected" && item["isDefault"] == true));
    }
    #[test]
    fn codex_snapshot_has_the_app_order_efforts_and_speed_contract() {
        let value: Value = serde_json::from_str(BUNDLED_CODEX_MODELS).unwrap();
        let models = parse_catalog(&value, true).unwrap();
        assert_eq!(models[0]["model"], DEFAULT_MODEL);
        assert_eq!(models[0]["defaultReasoningEffort"], "low");
        assert_eq!(
            models[0]["supportedReasoningEfforts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value["reasoningEffort"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "xhigh", "max", "ultra"]
        );
        assert_eq!(models[0]["serviceTiers"][0]["id"], "priority");
        let luna = models
            .iter()
            .find(|model| model["model"] == "gpt-6-luna")
            .unwrap();
        assert!(!luna["supportedReasoningEfforts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effort| effort["reasoningEffort"] == "ultra"));
        assert_eq!(resolve_effort(luna, "ultra"), "medium");
        assert_eq!(normalize_tier("fast"), Some("priority".into()));
        assert_eq!(normalize_tier("auto"), None);
        assert_eq!(
            models
                .iter()
                .filter(|model| model["hidden"] == false)
                .count(),
            7
        );
    }
}
