//! Streaming client for the Responses API (`POST {base}/responses`, SSE).
//! Works with both API-key auth (api.openai.com/v1) and ChatGPT auth (chatgpt.com/backend-api/codex).

use crate::auth::Auth;
use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

pub struct Client {
    http: reqwest::Client,
    pub auth: Auth,
    pub service_tier: Option<String>,
    #[cfg(test)]
    test_endpoint: Option<String>,
}

#[derive(Default)]
pub struct Turn {
    /// Every completed output item (messages, reasoning, function calls), to be replayed as history.
    pub items: Vec<Value>,
    pub calls: Vec<FunctionCall>,
    pub tokens_used: Option<u64>,
}

pub struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

pub enum StreamDelta<'a> {
    Text(&'a str),
    WebSearch(&'a Value),
    OutputMessage(&'a Value),
    ReasoningStart(&'a str),
    ReasoningDelta { id: &'a str, delta: &'a str },
    ReasoningDone { id: &'a str, text: String },
}

fn normalize_effort(effort: &str) -> Result<&'static str> {
    match effort.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok("auto"),
        "none" => Ok("none"),
        "minimal" => Ok("minimal"),
        "low" | "light" => Ok("low"),
        "medium" => Ok("medium"),
        "high" => Ok("high"),
        "xhigh" | "extra_high" | "extra high" => Ok("xhigh"),
        "max" => Ok("max"),
        "ultra" => Ok("ultra"),
        _ => bail!("unsupported reasoning effort: {effort}"),
    }
}

/// Keep network chunks as bytes. UTF-8 is decoded only after an entire SSE
/// event has arrived, including its terminating blank line.
#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<Value>> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(end) = self.event_end() {
            if end > MAX_EVENT_BYTES {
                bail!("SSE event exceeds the size limit");
            }
            let frame: Vec<u8> = self.pending.drain(..end).collect();
            let text = std::str::from_utf8(&frame).context("SSE event contains invalid UTF-8")?;
            let data = text
                .lines()
                .filter_map(|line| {
                    line.strip_prefix("data:")
                        .map(|value| value.strip_prefix(' ').unwrap_or(value))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            events.push(serde_json::from_str(&data).context("invalid JSON in SSE event")?);
        }
        if self.pending.len() > MAX_EVENT_BYTES {
            bail!("SSE event exceeds the size limit");
        }
        Ok(events)
    }

    /// Accept both LF and CRLF line endings, even when the delimiter is split
    /// across HTTP chunks. A blank line dispatches the event.
    fn event_end(&self) -> Option<usize> {
        let mut line_start = 0;
        for (index, byte) in self.pending.iter().enumerate() {
            if *byte == b'\n' {
                if index == line_start
                    || (index == line_start + 1 && self.pending[line_start] == b'\r')
                {
                    return Some(index + 1);
                }
                line_start = index + 1;
            }
        }
        None
    }
}

impl Client {
    pub fn new(auth: Auth) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("build HTTP client"),
            auth,
            service_tier: None,
            #[cfg(test)]
            test_endpoint: None,
        }
    }

    pub fn unauthenticated() -> Self {
        Self::new(Auth::Unavailable)
    }
    #[cfg(test)]
    pub(crate) fn test_at(base_url: &str) -> Self {
        let mut client = Self::new(Auth::ApiKey("isolated-test-key".into()));
        client.http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        client.test_endpoint = Some(base_url.into());
        client
    }

    pub async fn stream_turn(
        &mut self,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        self.stream_turn_search(model, effort, instructions, input, tools, false, on_delta)
            .await
    }

    pub(crate) async fn stream_turn_search(
        &mut self,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        require_search: bool,
        on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        #[cfg(test)]
        let base = self
            .test_endpoint
            .clone()
            .unwrap_or_else(|| self.auth.base_url());
        #[cfg(not(test))]
        let base = self.auth.base_url();
        self.stream_turn_with_limits(
            &base,
            model,
            effort,
            instructions,
            input,
            tools,
            STREAM_IDLE_TIMEOUT,
            None,
            require_search,
            on_delta,
        )
        .await
    }

    async fn stream_turn_at(
        &mut self,
        base_url: &str,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        idle_timeout: Duration,
        on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        self.stream_turn_with_limits(
            base_url,
            model,
            effort,
            instructions,
            input,
            tools,
            idle_timeout,
            None,
            false,
            on_delta,
        )
        .await
    }

    /// Bounded, text-oriented consumers can cap the entire wire response and model output.
    /// Error payloads are never read or reflected by this entry point.
    pub(crate) async fn stream_turn_bounded(
        &mut self,
        base_url: &str,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        max_response_bytes: usize,
        max_output_tokens: u32,
        on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        if !(1024..=32 * 1024 * 1024).contains(&max_response_bytes)
            || !(1..=32768).contains(&max_output_tokens)
        {
            bail!("Invalid bounded Responses request limits");
        }
        self.stream_turn_with_limits(
            base_url,
            model,
            effort,
            instructions,
            input,
            tools,
            STREAM_IDLE_TIMEOUT,
            Some((max_response_bytes, max_output_tokens)),
            false,
            on_delta,
        )
        .await
    }

    async fn stream_turn_with_limits(
        &mut self,
        base_url: &str,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        idle_timeout: Duration,
        limits: Option<(usize, u32)>,
        require_search: bool,
        on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        let declarations = tools
            .as_array()
            .context("Responses tools must be an array")?;
        let search_enabled = declarations.iter().any(|tool| tool["type"] == "web_search");
        if limits.is_some() && !declarations.is_empty() {
            bail!("Bounded document requests do not allow tools");
        }
        if require_search && !search_enabled {
            bail!("Required web search needs a declared web_search tool");
        }
        if matches!(self.auth, Auth::Unavailable) {
            bail!("not signed in: run `mcodex login` or set OPENAI_API_KEY");
        }
        let bounded_http = if limits.is_some() {
            Some(
                reqwest::Client::builder()
                    .connect_timeout(CONNECT_TIMEOUT)
                    .timeout(REQUEST_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .context("building bounded Responses client")?,
            )
        } else {
            None
        };
        let http = bounded_http.as_ref().unwrap_or(&self.http);
        let api_effort = normalize_effort(effort)?;
        self.auth.ensure_fresh(http).await?;
        // store=false everywhere: history is replayed client-side (required for ChatGPT auth).
        let mut body = json!({
            "model": model,
            "instructions": instructions,
            "input": input,
            "tools": tools,
            "tool_choice": "auto",
            "parallel_tool_calls": false,
            "reasoning": { "effort": api_effort, "summary": "auto" },
            "stream": true,
            "store": false,
            "include": ["reasoning.encrypted_content"],
        });
        if search_enabled {
            body["include"]
                .as_array_mut()
                .unwrap()
                .push(json!("web_search_call.action.sources"));
        }
        if require_search {
            body["tool_choice"] = json!({"type":"web_search"});
        }
        if api_effort == "auto" {
            body.as_object_mut().unwrap().remove("reasoning");
        }
        if let Some(tier) = self.service_tier.as_deref().and_then(crate::models::normalize_tier) {
            body["service_tier"] = json!(tier);
        }
        if let Some((_, tokens)) = limits {
            body["max_output_tokens"] = json!(tokens);
        }
        let mut req = http
            .post(format!("{}/responses", base_url.trim_end_matches('/')))
            .header("originator", "codex_cli_rs")
            .header("accept", "text/event-stream");
        req = match &self.auth {
            Auth::Unavailable => bail!("not signed in: run `mcodex login` or set OPENAI_API_KEY"),
            Auth::ApiKey(key) => req.bearer_auth(key),
            Auth::ChatGpt {
                access, account_id, ..
            } => req
                .bearer_auth(access)
                .header("chatgpt-account-id", account_id)
                .header("OpenAI-Beta", "responses=experimental"),
        };
        let response = tokio::time::timeout(idle_timeout, req.json(&body).send())
            .await
            .context("Responses API request timed out waiting for response headers")?
            .context("sending Responses API request")?;
        if !response.status().is_success() {
            let status = response.status();
            if limits.is_some() {
                bail!("Responses API request failed (HTTP {})", status.as_u16());
            }
            let detail = tokio::time::timeout(idle_timeout, response.text())
                .await
                .context("Responses API error body timed out")??;
            bail!("API error {status}: {}", detail);
        }
        if let Some((max, _)) = limits {
            if response.content_length().is_some_and(|n| n > max as u64) {
                bail!("Responses API response exceeds the size limit");
            }
        }
        receive_stream(
            response,
            idle_timeout,
            limits.map(|l| l.0),
            search_enabled,
            require_search,
            on_delta,
        )
        .await
    }
}

struct OutputCollector {
    items: Vec<OutputItem>,
    notices: HashMap<String, Value>,
    search_progress: HashMap<String, Value>,
    annotations: HashMap<String, BTreeMap<(usize, usize), Value>>,
    reasoning_started: HashSet<String>,
    item_events: usize,
    namespace: String,
}
impl Default for OutputCollector {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            notices: HashMap::new(),
            search_progress: HashMap::new(),
            annotations: HashMap::new(),
            reasoning_started: HashSet::new(),
            item_events: 0,
            namespace: format!("response_{:032x}", rand::random::<u128>()),
        }
    }
}
struct OutputItem {
    key: String,
    item: Value,
    index: Option<u64>,
    done: bool,
    synthetic: bool,
}
impl OutputCollector {
    fn remove_local_fields(item: &mut Value) {
        if let Some(fields) = item.as_object_mut() {
            fields.retain(|key, _| !key.starts_with("_mcodex_"));
        }
    }
    fn merge_received(old: &Value, new: &mut Value) {
        if let (Some(old), Some(new)) = (old.as_object(), new.as_object_mut()) {
            for (key, value) in old {
                if key != "status" && (!new.contains_key(key) || new[key].is_null()) {
                    new.insert(key.clone(), value.clone());
                }
            }
        }
        if old["type"] == "web_search_call" {
            if new.get("action").is_some() {
                Self::merge_received(&old["action"], &mut new["action"]);
            }
            if new["action"]["sources"]
                .as_array()
                .is_some_and(Vec::is_empty)
                && old["action"]["sources"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
            {
                new["action"]["sources"] = old["action"]["sources"].clone();
            }
        }
        if old["type"] == "message" {
            if let (Some(before), Some(after)) =
                (old["content"].as_array(), new["content"].as_array_mut())
            {
                if after.is_empty() {
                    *after = before.clone();
                } else {
                    for (before, after) in before.iter().zip(after) {
                        if before["type"] == after["type"] && before["text"] == after["text"] {
                            if before["annotations"]
                                .as_array()
                                .is_some_and(|v| !v.is_empty())
                                && after
                                    .get("annotations")
                                    .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
                            {
                                after["annotations"] = before["annotations"].clone();
                            }
                        }
                    }
                }
            }
        }
    }
    fn validate_kind(item: &Value, search_enabled: bool) -> Result<()> {
        match item["type"].as_str() {
            Some("message" | "reasoning" | "function_call") => Ok(()),
            Some("web_search_call") if search_enabled => crate::web_search::project_call(item)
                .map(|_| ())
                .map_err(anyhow::Error::msg),
            Some("web_search_call") => {
                bail!("The provider returned web search without a requested web_search tool")
            }
            _ => bail!("The provider returned an undeclared hosted output type"),
        }
    }
    fn merge_annotations(&self, item: &mut Value) -> Result<()> {
        let Some(id) = item["id"].as_str() else {
            return Ok(());
        };
        let Some(pending) = self.annotations.get(id) else {
            return Ok(());
        };
        let Some(parts) = item["content"].as_array_mut() else {
            return Ok(());
        };
        for ((part_index, annotation_index), annotation) in pending {
            let part = parts
                .get_mut(*part_index)
                .context("Citation refers to an unavailable message part")?;
            if part.get("annotations").is_none() {
                part["annotations"] = json!([]);
            }
            let annotations = part["annotations"]
                .as_array_mut()
                .context("Invalid message annotations")?;
            if *annotation_index > annotations.len() {
                bail!("Citation annotations contain a missing index");
            }
            if *annotation_index == annotations.len() {
                annotations.push(annotation.clone());
            }
        }
        Ok(())
    }
    fn upsert(&mut self, mut item: Value, index: Option<u64>, done: bool) -> Result<usize> {
        Self::remove_local_fields(&mut item);
        if index.is_some_and(|n| n >= 4096) {
            bail!("Responses API response exceeds the item limit");
        }
        if item["type"] == "message" && done {
            self.merge_annotations(&mut item)?;
        }
        let id = item["id"].as_str().filter(|s| !s.is_empty());
        let call_id = (item["type"] == "function_call")
            .then(|| item["call_id"].as_str())
            .flatten();
        let existing = self
            .items
            .iter()
            .position(|old| {
                id.is_some_and(|id| old.item["id"].as_str() == Some(id))
                    || call_id.is_some_and(|id| {
                        old.item["type"] == "function_call"
                            && old.item["call_id"].as_str() == Some(id)
                    })
                    || index.is_some() && old.index == index
            })
            .or_else(|| {
                index.and_then(|index| {
                    self.items
                        .get(index as usize)
                        .filter(|old| {
                            old.index.is_none()
                                && old.item["id"].is_null()
                                && old.item["type"] == item["type"]
                        })
                        .map(|_| index as usize)
                })
            })
            .or_else(|| {
                if index.is_none() {
                    self.items.iter().position(|old| {
                        !old.done
                            && old.item["id"].is_null()
                            && old.item["type"] == item["type"]
                            && (done || id.is_none() && old.item == item)
                    })
                } else {
                    None
                }
            });
        if let Some(at) = existing {
            let old = &mut self.items[at];
            if old.item["type"] != item["type"] {
                bail!("Responses output changed item type at the same index");
            }
            if old.done && !done {
                return Ok(at);
            }
            if !old.synthetic {
                Self::merge_received(&old.item, &mut item);
            }
            old.item = item;
            old.index = index.or(old.index);
            old.done |= done;
            old.synthetic = false;
            return Ok(at);
        }
        if self.items.len() >= 4096 {
            bail!("Responses API response exceeds the item limit");
        }
        let key = format!("{}:item:{}", self.namespace, self.items.len());
        self.items.push(OutputItem {
            key,
            item,
            index,
            done,
            synthetic: false,
        });
        Ok(self.items.len() - 1)
    }
    fn activate(&self, at: usize, on_delta: &mut impl FnMut(StreamDelta<'_>)) {
        let entry = &self.items[at];
        let mut item = json!({"type":"message","role":"assistant","status":"in_progress","content":[],"_mcodex_item_key":entry.key,"_mcodex_activate":true});
        if let Some(id) = entry.item.get("id") {
            item["id"] = id.clone();
        }
        on_delta(StreamDelta::OutputMessage(&item));
    }
    fn text_item(&mut self, event: &Value) -> Result<usize> {
        let id = event["item_id"].as_str();
        let index = event["output_index"].as_u64();
        let existing = self
            .items
            .iter()
            .position(|entry| {
                id.is_some_and(|id| entry.item["id"] == id)
                    || index.is_some() && entry.index == index
            })
            .or_else(|| {
                if id.is_none() && index.is_none() {
                    self.items
                        .iter()
                        .rposition(|entry| entry.item["type"] == "message" && !entry.done)
                } else {
                    None
                }
            });
        if let Some(at) = existing {
            if self.items[at].item["type"] != "message" || self.items[at].done {
                bail!("Text delta refers to an unavailable or completed message");
            }
            return Ok(at);
        }
        let mut item =
            json!({"type":"message","role":"assistant","status":"in_progress","content":[]});
        if let Some(id) = id {
            item["id"] = json!(id);
        }
        let at = self.upsert(item, index, false)?;
        self.items[at].synthetic = true;
        Ok(at)
    }
    fn emit_search(
        &mut self,
        mut item: Value,
        on_delta: &mut impl FnMut(StreamDelta<'_>),
    ) -> Result<()> {
        Self::remove_local_fields(&mut item);
        crate::web_search::project_call(&item).map_err(anyhow::Error::msg)?;
        let id = item["id"].as_str().unwrap().to_string();
        if let Some(previous) = self.search_progress.get(&id) {
            let earlier = matches!(item["status"].as_str(), Some("in_progress" | "searching"));
            let advanced = matches!(
                previous["status"].as_str(),
                Some("completed" | "failed" | "incomplete")
            ) || previous["status"] == "searching"
                && item["status"] == "in_progress";
            if earlier && advanced {
                return Ok(());
            }
            if previous == &item {
                return Ok(());
            }
        }
        self.search_progress.insert(id.clone(), item.clone());
        let mut event_item = item;
        event_item["_mcodex_item_key"] = json!(format!("{}:search:{id}", self.namespace));
        on_delta(StreamDelta::WebSearch(&event_item));
        Ok(())
    }
    fn emit_done(
        &mut self,
        at: usize,
        bounded: bool,
        on_delta: &mut impl FnMut(StreamDelta<'_>),
    ) -> Result<()> {
        let item = self.items[at].item.clone();
        let key = self.items[at].key.clone();
        if item["type"] == "web_search_call" {
            return self.emit_search(item, on_delta);
        }
        if self.notices.get(&key) == Some(&item) {
            return Ok(());
        }
        if item["type"] == "message" && !bounded {
            crate::web_search::project_message(&item).map_err(anyhow::Error::msg)?;
            let mut event_item = item.clone();
            event_item
                .as_object_mut()
                .unwrap()
                .remove("_mcodex_activate");
            event_item["_mcodex_item_key"] = json!(key);
            on_delta(StreamDelta::OutputMessage(&event_item));
        } else if item["type"] == "reasoning" {
            let id = item["id"].as_str().unwrap_or("");
            let text = item["summary"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .unwrap_or_default();
            on_delta(StreamDelta::ReasoningDone { id, text });
        }
        self.notices.insert(key, item);
        Ok(())
    }
    fn finish(mut self, bounded: bool, require_search: bool) -> Result<Turn> {
        for (id, _) in &self.search_progress {
            if !self.items.iter().any(|entry| {
                entry.done && entry.item["type"] == "web_search_call" && entry.item["id"] == *id
            }) {
                bail!("Web search ended without a complete output item");
            }
        }
        let mut turn = Turn::default();
        let mut calls = HashMap::<String, (String, String)>::new();
        self.items
            .sort_by_key(|entry| (entry.index.is_none(), entry.index.unwrap_or(0)));
        for entry in self.items {
            let item = entry.item;
            match item["type"].as_str() {
                Some("web_search_call") => {
                    if !entry.done
                        || matches!(item["status"].as_str(), Some("in_progress" | "searching"))
                    {
                        bail!("Web search ended with an incomplete output item");
                    }
                }
                Some("function_call") => {
                    if !bounded && !entry.done {
                        bail!("The model left an incomplete function call");
                    }
                    let id = item["call_id"].as_str().unwrap_or_default().to_string();
                    let name = item["name"].as_str().unwrap_or_default().to_string();
                    let arguments = item["arguments"].as_str().unwrap_or("{}").to_string();
                    if !bounded
                        && (id.is_empty() || name.is_empty() || !item["arguments"].is_string())
                    {
                        bail!("The model returned an invalid function call");
                    }
                    if let Some(previous) = calls.get(&id) {
                        if previous != &(name.clone(), arguments.clone()) {
                            bail!("The model returned conflicting function calls with the same id");
                        }
                    } else {
                        calls.insert(id.clone(), (name.clone(), arguments.clone()));
                        turn.calls.push(FunctionCall {
                            call_id: id,
                            name,
                            arguments,
                        });
                    }
                }
                Some("message" | "reasoning") if !entry.done => continue,
                _ => {}
            }
            turn.items.push(item);
        }
        if require_search
            && !turn
                .items
                .iter()
                .any(|item| item["type"] == "web_search_call")
        {
            bail!("Required web search returned no search call");
        }
        Ok(turn)
    }
}

async fn receive_stream(
    response: reqwest::Response,
    idle_timeout: Duration,
    max_bytes: Option<usize>,
    search_enabled: bool,
    require_search: bool,
    mut on_delta: impl FnMut(StreamDelta<'_>),
) -> Result<Turn> {
    let mut output = OutputCollector::default();
    let mut decoder = SseDecoder::default();
    let mut reasoning = String::new();
    let mut stream = response.bytes_stream();
    let mut total_bytes = 0usize;
    loop {
        let chunk = tokio::time::timeout(idle_timeout, stream.next())
            .await
            .context("Responses API stream timed out waiting for data")?;
        let Some(chunk) = chunk else {
            bail!("Responses API stream ended before response.completed");
        };
        let chunk = chunk.context("reading Responses API stream")?;
        total_bytes = total_bytes
            .checked_add(chunk.len())
            .context("Responses API byte count overflow")?;
        if max_bytes.is_some_and(|max| total_bytes > max) {
            bail!("Responses API response exceeds the size limit");
        }
        for event in decoder.push(&chunk)? {
            match event["type"].as_str().unwrap_or("") {
                "response.output_item.added" | "response.output_item.done" => {
                    output.item_events += 1;
                    if output.item_events > 8192 || max_bytes.is_some() && output.item_events > 4096
                    {
                        bail!("Responses API response exceeds the item limit");
                    }
                    let item = event["item"].clone();
                    OutputCollector::validate_kind(&item, search_enabled)?;
                    let done = event["type"] == "response.output_item.done";
                    let at = output.upsert(item, event["output_index"].as_u64(), done)?;
                    if done {
                        output.emit_done(at, max_bytes.is_some(), &mut on_delta)?;
                    } else if event["item"]["type"] == "message" {
                        if max_bytes.is_none() {
                            output.activate(at, &mut on_delta);
                        }
                    } else if event["item"]["type"] == "reasoning" {
                        if let Some(id) = event["item"]["id"].as_str() {
                            reasoning = id.into();
                            if output.reasoning_started.insert(id.into()) {
                                on_delta(StreamDelta::ReasoningStart(id));
                            }
                        }
                    } else if event["item"]["type"] == "web_search_call" {
                        output.emit_search(event["item"].clone(), &mut on_delta)?;
                    }
                }
                "response.web_search_call.in_progress"
                | "response.web_search_call.searching"
                | "response.web_search_call.completed" => {
                    if !search_enabled {
                        bail!(
                            "The provider returned web search without a requested web_search tool"
                        );
                    }
                    let id = event["item_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .context("Web search progress has no item id")?;
                    let mut item = output
                        .items
                        .iter()
                        .find(|entry| entry.item["id"] == id)
                        .map(|entry| entry.item.clone())
                        .unwrap_or_else(|| json!({"type":"web_search_call","id":id}));
                    item["status"] =
                        json!(event["type"].as_str().unwrap().rsplit('.').next().unwrap());
                    output.emit_search(item, &mut on_delta)?;
                }
                "response.output_text.annotation.added" => {
                    let id = event["item_id"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 256)
                        .context("Citation event has no valid item id")?;
                    let part = event["content_index"]
                        .as_u64()
                        .filter(|n| *n < 1024)
                        .context("Invalid citation content index")?
                        as usize;
                    let index = event["annotation_index"]
                        .as_u64()
                        .filter(|n| *n < 1024)
                        .context("Invalid citation annotation index")?
                        as usize;
                    let annotation = event["annotation"].clone();
                    crate::web_search::project_message(&json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"","annotations":[annotation.clone()]}]})).map_err(anyhow::Error::msg)?;
                    if output.annotations.len() >= 4096 && !output.annotations.contains_key(id) {
                        bail!("Responses API response exceeds the item limit");
                    }
                    output
                        .annotations
                        .entry(id.into())
                        .or_default()
                        .insert((part, index), annotation);
                    if let Some(at) = output
                        .items
                        .iter()
                        .position(|entry| entry.done && entry.item["id"] == id)
                    {
                        let item = output.items[at].item.clone();
                        let at = output.upsert(item, event["output_index"].as_u64(), true)?;
                        output.emit_done(at, max_bytes.is_some(), &mut on_delta)?;
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    if let Some(delta) = event["delta"].as_str() {
                        let id = event["item_id"].as_str().unwrap_or(&reasoning);
                        on_delta(StreamDelta::ReasoningDelta { id, delta });
                    }
                }
                "response.reasoning_summary_part.added" => {
                    let id = event["item_id"].as_str().unwrap_or(&reasoning);
                    on_delta(StreamDelta::ReasoningDelta { id, delta: "\n" });
                }
                "response.output_text.delta" => {
                    if let Some(delta) = event["delta"].as_str() {
                        let at = output.text_item(&event)?;
                        if max_bytes.is_none() {
                            output.activate(at, &mut on_delta);
                        }
                        on_delta(StreamDelta::Text(delta));
                    }
                }
                "response.completed" => {
                    if event["response"]["status"]
                        .as_str()
                        .is_some_and(|s| s != "completed")
                    {
                        if max_bytes.is_some() {
                            bail!("Responses API did not complete successfully");
                        }
                        bail!("Responses API did not complete successfully");
                    }
                    if let Some(final_output) =
                        event["response"].get("output").filter(|v| !v.is_null())
                    {
                        let items = final_output
                            .as_array()
                            .context("Responses API returned invalid final output")?;
                        if items.len() > 4096 {
                            bail!("Responses API response exceeds the item limit");
                        }
                        for (index, item) in items.iter().enumerate() {
                            OutputCollector::validate_kind(item, search_enabled)?;
                            let at = output.upsert(item.clone(), Some(index as u64), true)?;
                            output.emit_done(at, max_bytes.is_some(), &mut on_delta)?;
                        }
                    }
                    let mut turn = output.finish(max_bytes.is_some(), require_search)?;
                    turn.tokens_used = event["response"]["usage"]["total_tokens"].as_u64();
                    return Ok(turn);
                }
                "response.failed" | "response.incomplete" | "response.cancelled" | "error" => {
                    if max_bytes.is_some() {
                        bail!("Responses API stream did not complete successfully");
                    }
                    bail!("Responses API stream error: {event}");
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    struct MockServer {
        base_url: String,
        request: oneshot::Receiver<Value>,
        runner: tokio::task::JoinHandle<()>,
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.runner.abort();
        }
    }

    async fn mock_server(status: &str, chunks: Vec<Vec<u8>>, keep_open: Duration) -> MockServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let (tx, request) = oneshot::channel();
        let runner = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request_bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let size = socket.read(&mut buffer).await.unwrap();
                if size == 0 {
                    return;
                }
                request_bytes.extend_from_slice(&buffer[..size]);
                if let Some(index) = request_bytes
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                {
                    let headers = std::str::from_utf8(&request_bytes[..index]).unwrap();
                    assert!(headers.starts_with("POST /responses HTTP/1.1"));
                    let size: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    if request_bytes.len() >= index + 4 + size {
                        let body =
                            serde_json::from_slice(&request_bytes[index + 4..index + 4 + size])
                                .unwrap();
                        let _ = tx.send(body);
                        break;
                    }
                }
            }
            if status.is_empty() {
                tokio::time::sleep(keep_open).await;
                return;
            }
            let headers = format!("HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
            if socket.write_all(headers.as_bytes()).await.is_err() {
                return;
            }
            for chunk in chunks {
                if socket
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                if socket.flush().await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            tokio::time::sleep(keep_open).await;
            let _ = socket.write_all(b"0\r\n\r\n").await;
        });
        MockServer {
            base_url: format!("http://{address}"),
            request,
            runner,
        }
    }

    fn frame(value: Value) -> Vec<u8> {
        format!("data: {value}\r\n\r\n").into_bytes()
    }

    fn completed() -> Vec<u8> {
        frame(json!({"type": "response.completed", "response": {"status": "completed"}}))
    }

    fn test_client() -> Client {
        let mut client = Client::new(Auth::ApiKey("test-key".into()));
        // Loopback mocks must not use a machine's configured HTTP proxy.
        client.http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        client
    }

    async fn call(client: &mut Client, server: &MockServer, effort: &str) -> Result<Turn> {
        client
            .stream_turn_at(
                &server.base_url,
                "test",
                effort,
                "instructions",
                &[],
                &json!([]),
                Duration::from_millis(200),
                |_| {},
            )
            .await
    }

    #[tokio::test]
    async fn bounded_text_calls_send_output_cap_and_stop_on_wire_or_item_limits() {
        let mut server = mock_server(
            "200 OK",
            vec![
                frame(json!({"type":"response.output_text.delta","delta":"安全片段"})),
                completed(),
            ],
            Duration::ZERO,
        )
        .await;
        let mut text = String::new();
        let mut client = test_client();
        client
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "instructions",
                &[],
                &json!([]),
                4096,
                77,
                |delta| {
                    if let StreamDelta::Text(s) = delta {
                        text.push_str(s);
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(text, "安全片段");
        let request = (&mut server.request).await.unwrap();
        assert_eq!(request["max_output_tokens"], 77);
        assert!(request.get("reasoning").is_none());
        let server = mock_server(
            "200 OK",
            vec![
                frame(json!({"type":"response.output_text.delta","delta":"x".repeat(2048)})),
                completed(),
            ],
            Duration::ZERO,
        )
        .await;
        let error = test_client()
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &json!([]),
                1024,
                77,
                |_| {},
            )
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("size limit"));
        let mut bytes = Vec::new();
        for _ in 0..4097 {
            bytes.extend(frame(
                json!({"type":"response.output_item.done","item":{"type":"message"}}),
            ));
        }
        bytes.extend(completed());
        let server = mock_server("200 OK", vec![bytes], Duration::ZERO).await;
        let error = test_client()
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &json!([]),
                2 * 1024 * 1024,
                77,
                |_| {},
            )
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("item limit"));
    }

    #[tokio::test]
    async fn bounded_failures_never_reflect_http_or_sse_error_payloads() {
        for (status, body) in [
            ("403 Forbidden", b"private-fixture-key".to_vec()),
            (
                "200 OK",
                frame(json!({"type":"response.failed","error":{"message":"private-fixture-key"}})),
            ),
            (
                "200 OK",
                frame(
                    json!({"type":"response.completed","response":{"status":"private-fixture-key"}}),
                ),
            ),
        ] {
            let server = mock_server(status, vec![body], Duration::ZERO).await;
            let error = test_client()
                .stream_turn_bounded(
                    &server.base_url,
                    "test",
                    "auto",
                    "",
                    &[],
                    &json!([]),
                    4096,
                    77,
                    |_| {},
                )
                .await
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("private-fixture-key"));
            assert!(error.contains(if status.starts_with("403") {
                "HTTP 403"
            } else {
                "successfully"
            }));
        }
        let server = mock_server(
            "302 Found\r\nLocation: http://127.0.0.1:1/never-follow",
            vec![],
            Duration::ZERO,
        )
        .await;
        let error = test_client()
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &json!([]),
                4096,
                77,
                |_| {},
            )
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("HTTP 302"));
    }

    #[tokio::test]
    async fn bounded_calls_read_final_only_output_and_never_erase_observed_tools() {
        let message = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"final only"}]});
        let tool = json!({"type":"function_call","call_id":"tool-1","name":"not-declared","arguments":"{}"});
        for (prefix, output, expected_calls) in [
            (vec![], vec![message.clone()], 0),
            (vec![], vec![tool.clone()], 1),
            (
                vec![frame(
                    json!({"type":"response.output_item.done","item":tool.clone()}),
                )],
                vec![message.clone()],
                1,
            ),
            (
                vec![frame(
                    json!({"type":"response.output_item.added","item":tool}),
                )],
                vec![message.clone()],
                1,
            ),
        ] {
            let mut chunks = prefix;
            chunks.push(frame(json!({"type":"response.completed","response":{"status":"completed","output":output}})));
            let server = mock_server("200 OK", chunks, Duration::ZERO).await;
            let turn = test_client()
                .stream_turn_bounded(
                    &server.base_url,
                    "test",
                    "auto",
                    "",
                    &[],
                    &json!([]),
                    16 * 1024,
                    77,
                    |_| {},
                )
                .await
                .unwrap();
            assert_eq!(turn.calls.len(), expected_calls);
            assert!(!turn.items.is_empty());
            if expected_calls == 0 {
                assert_eq!(turn.items[0], message);
            } else {
                assert!(turn.items.iter().any(|i| i["type"] == "function_call"));
            }
        }
    }

    #[test]
    fn decoder_preserves_unicode_for_every_byte_split_and_both_line_endings() {
        for ending in ["\n\n", "\r\n\r\n"] {
            let event = format!(
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"中文🙂\"}}{ending}"
            )
            .into_bytes();
            for split in 0..=event.len() {
                let mut decoder = SseDecoder::default();
                let mut values = decoder.push(&event[..split]).unwrap();
                values.extend(decoder.push(&event[split..]).unwrap());
                assert_eq!(values.len(), 1);
                assert_eq!(values[0]["delta"], "中文🙂");
            }
        }
    }

    #[test]
    fn decoder_handles_comments_multiline_data_and_rejects_invalid_utf8() {
        let mut decoder = SseDecoder::default();
        let values = decoder.push(b": heartbeat\r\n\r\ndata: {\"type\":\n\ndata: \"ignored\"}\n\n");
        assert!(values.is_err());
        let mut decoder = SseDecoder::default();
        let values = decoder
            .push(b": heartbeat\r\n\r\ndata: {\"type\":\r\ndata: \"response.completed\"}\r\n\r\n")
            .unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0]["type"], "response.completed");
        assert!(SseDecoder::default().push(b"data: \xff\n\n").is_err());
    }

    #[tokio::test]
    async fn local_http_stream_preserves_chinese_split_across_chunks() {
        let delta = frame(json!({"type": "response.output_text.delta", "delta": "中文🙂"}));
        let split = delta
            .windows(3)
            .position(|bytes| bytes == "中".as_bytes())
            .unwrap()
            + 1;
        let item = json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "中文🙂"}]});
        let chunks = vec![
            delta[..split].to_vec(),
            delta[split..split + 1].to_vec(),
            delta[split + 1..].to_vec(),
            frame(json!({"type": "response.output_item.done", "item": item})),
            completed(),
        ];
        let server = mock_server("200 OK", chunks, Duration::ZERO).await;
        let mut client = test_client();
        let mut text = String::new();
        let turn = client
            .stream_turn_at(
                &server.base_url,
                "test",
                "medium",
                "",
                &[],
                &json!([]),
                Duration::from_secs(1),
                |delta| {
                    if let StreamDelta::Text(part) = delta {
                        text.push_str(part);
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(text, "中文🙂");
        assert_eq!(turn.items, vec![item]);
    }

    #[tokio::test]
    async fn sends_actual_reasoning_effort_and_optional_service_tier() {
        for (effort, expected) in [
            ("none", "none"),
            ("minimal", "minimal"),
            ("low", "low"),
            ("medium", "medium"),
            ("high", "high"),
            ("xhigh", "xhigh"),
            ("light", "low"),
            ("extra_high", "xhigh"),
            ("max", "max"),
            ("ultra", "ultra"),
        ] {
            let mut server = mock_server("200 OK", vec![completed()], Duration::ZERO).await;
            let mut client = test_client();
            client.service_tier = Some("priority".into());
            call(&mut client, &server, effort).await.unwrap();
            let request = (&mut server.request).await.unwrap();
            assert_eq!(request["reasoning"]["effort"], expected);
            assert_eq!(request["service_tier"], "priority");
        }
        let mut server = mock_server("200 OK", vec![completed()], Duration::ZERO).await;
        call(&mut test_client(), &server, "medium").await.unwrap();
        assert!((&mut server.request)
            .await
            .unwrap()
            .get("service_tier")
            .is_none());
        for (setting,expected) in [("auto",None),("default",None),("fast",Some("priority"))] {
            let mut server=mock_server("200 OK",vec![completed()],Duration::ZERO).await;
            let mut client=test_client();client.service_tier=Some(setting.into());call(&mut client,&server,"low").await.unwrap();
            let request=(&mut server.request).await.unwrap();assert_eq!(request.get("service_tier").and_then(Value::as_str),expected);
        }
        assert!(normalize_effort("persistent").is_err());
        let mut server = mock_server("200 OK", vec![completed()], Duration::ZERO).await;
        call(&mut test_client(), &server, "auto").await.unwrap();
        assert!((&mut server.request)
            .await
            .unwrap()
            .get("reasoning")
            .is_none());
    }

    #[tokio::test]
    async fn eof_incomplete_failed_and_http_errors_do_not_report_success() {
        for (status, chunks, expected) in [
            (
                "200 OK",
                vec![frame(
                    json!({"type": "response.output_text.delta", "delta": "partial"}),
                )],
                "before response.completed",
            ),
            (
                "200 OK",
                vec![b"data: [DONE]\n\n".to_vec()],
                "before response.completed",
            ),
            (
                "200 OK",
                vec![frame(
                    json!({"type": "response.incomplete", "response": {"status": "incomplete"}}),
                )],
                "response.incomplete",
            ),
            (
                "200 OK",
                vec![frame(
                    json!({"type": "response.failed", "response": {"error": {"message": "failed"}}}),
                )],
                "response.failed",
            ),
            (
                "401 Unauthorized",
                vec![b"invalid credentials".to_vec()],
                "401 Unauthorized",
            ),
        ] {
            let server = mock_server(status, chunks, Duration::ZERO).await;
            let error = call(&mut test_client(), &server, "medium")
                .await
                .err()
                .unwrap();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    #[tokio::test]
    async fn idle_stream_times_out_and_completion_does_not_wait_for_disconnect() {
        let server = mock_server(
            "200 OK",
            vec![b": connected\n\n".to_vec()],
            Duration::from_secs(5),
        )
        .await;
        let mut client = test_client();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            client.stream_turn_at(
                &server.base_url,
                "test",
                "medium",
                "",
                &[],
                &json!([]),
                Duration::from_millis(30),
                |_| {},
            ),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("timed out"));

        let server = mock_server("200 OK", vec![completed()], Duration::from_secs(5)).await;
        tokio::time::timeout(Duration::from_secs(1), call(&mut client, &server, "medium"))
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn unavailable_auth_fails_without_network_access() {
        let mut client = Client::unauthenticated();
        let error = client
            .stream_turn_at(
                "http://127.0.0.1:1",
                "test",
                "medium",
                "",
                &[],
                &json!([]),
                Duration::from_millis(30),
                |_| {},
            )
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("not signed in"));
    }

    #[tokio::test]
    async fn missing_http_headers_and_stalled_error_body_are_bounded() {
        for (status, expected) in [
            ("", "timed out waiting for response headers"),
            ("401 Unauthorized", "error body timed out"),
        ] {
            let server = mock_server(status, vec![], Duration::from_secs(5)).await;
            let mut client = test_client();
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                client.stream_turn_at(
                    &server.base_url,
                    "test",
                    "medium",
                    "",
                    &[],
                    &json!([]),
                    Duration::from_millis(30),
                    |_| {},
                ),
            )
            .await
            .unwrap();
            assert!(format!("{:#}", result.err().unwrap()).contains(expected));
        }
    }

    fn search_call(status: &str) -> Value {
        json!({"type":"web_search_call","id":"ws_fixture","status":status})
    }
    fn raw_snapshot(item: &Value) -> Option<Value> {
        if item["_mcodex_activate"] == true {
            return None;
        }
        let mut item = item.clone();
        item.as_object_mut().unwrap().remove("_mcodex_item_key");
        Some(item)
    }
    fn cited_message() -> Value {
        json!({"type":"message","id":"msg_cited","role":"assistant","content":[{"type":"output_text","text":"中文🙂 answer","annotations":[{"type":"url_citation","url":"https://example.test/source","title":"Source","start_index":0,"end_index":4}]}]})
    }
    async fn search_at(
        client: &mut Client,
        server: &MockServer,
        mode: crate::web_search::SearchMode,
        required: bool,
        callback: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        let tools = mode
            .tool()
            .map(|tool| json!([tool]))
            .unwrap_or_else(|| json!([]));
        client
            .stream_turn_with_limits(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &tools,
                Duration::from_secs(2),
                None,
                required,
                callback,
            )
            .await
    }
    #[tokio::test]
    async fn hosted_search_streams_real_progress_and_keeps_sources_annotations_without_duplicates()
    {
        let mut call = search_call("completed");
        call["action"] = json!({"type":"search","queries":["latest 中文"],"sources":[{"type":"url","url":"https://example.test/source"}]});
        let message = cited_message();
        let mut without_annotations = message.clone();
        without_annotations["content"][0]["annotations"] = json!([]);
        let second = json!({"type":"message","id":"msg_second","role":"assistant","content":[{"type":"output_text","text":"First part","annotations":[]},{"type":"output_text","text":"Second part","annotations":[]}]});
        let mut chunks = Vec::new();
        for event in [
            json!({"type":"response.output_item.added","output_index":0,"item":search_call("in_progress")}),
            json!({"type":"response.output_item.added","output_index":0,"item":search_call("in_progress")}),
            json!({"type":"response.web_search_call.searching","item_id":"ws_fixture","output_index":0}),
            json!({"type":"response.web_search_call.searching","item_id":"ws_fixture","output_index":0}),
            json!({"type":"response.output_item.done","output_index":0,"item":call}),
            json!({"type":"response.output_item.done","output_index":0,"item":call}),
            json!({"type":"response.output_item.added","output_index":0,"item":search_call("in_progress")}),
            json!({"type":"response.output_text.delta","item_id":"msg_cited","delta":"中文🙂 answer"}),
            json!({"type":"response.output_text.annotation.added","item_id":"msg_cited","output_index":1,"content_index":0,"annotation_index":0,"annotation":message["content"][0]["annotations"][0]}),
            json!({"type":"response.output_item.done","output_index":1,"item":without_annotations}),
            json!({"type":"response.output_item.done","output_index":1,"item":without_annotations}),
            json!({"type":"response.completed","response":{"status":"completed","output":[search_call("completed"),message,second],"usage":{"total_tokens":17}}}),
        ] {
            chunks.push(frame(event));
        }
        let mut server = mock_server("200 OK", chunks, Duration::ZERO).await;
        let (mut progress, mut messages, mut streamed) = (Vec::new(), Vec::new(), String::new());
        let turn = search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Cached,
            false,
            |event| match event {
                StreamDelta::WebSearch(item) => progress.push(raw_snapshot(item).unwrap()),
                StreamDelta::OutputMessage(item) => {
                    if let Some(raw) = raw_snapshot(item) {
                        messages.push(raw);
                    }
                }
                StreamDelta::Text(text) => streamed.push_str(text),
                _ => {}
            },
        )
        .await
        .unwrap();
        assert_eq!(
            progress.len(),
            3,
            "duplicate/late provider events changed search state"
        );
        assert_eq!(progress[2], call);
        assert_eq!(messages, vec![message.clone(), second.clone()]);
        assert_eq!(streamed, "中文🙂 answer");
        assert_eq!(turn.items, vec![call, message, second]);
        assert!(turn.calls.is_empty());
        assert_eq!(turn.tokens_used, Some(17));
        let request = (&mut server.request).await.unwrap();
        assert_eq!(
            request["tools"],
            json!([{"type":"web_search","external_web_access":false}])
        );
        assert_eq!(
            request["include"],
            json!([
                "reasoning.encrypted_content",
                "web_search_call.action.sources"
            ])
        );
        assert_eq!(request["tool_choice"], "auto");
    }
    #[tokio::test]
    async fn final_only_search_and_messages_are_received_and_required_choice_does_not_fake_work() {
        let call = json!({"type":"web_search_call","id":"ws_final","status":"completed","action":{"type":"open_page","url":"https://example.test/real"}});
        let message = cited_message();
        let mut server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[call,message]}}))],Duration::ZERO).await;
        let (mut searches, mut messages) = (Vec::new(), Vec::new());
        let turn = search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Required,
            true,
            |event| match event {
                StreamDelta::WebSearch(v) => searches.push(raw_snapshot(v).unwrap()),
                StreamDelta::OutputMessage(v) => {
                    if let Some(raw) = raw_snapshot(v) {
                        messages.push(raw);
                    }
                }
                _ => {}
            },
        )
        .await
        .unwrap();
        assert_eq!(turn.items.len(), 2);
        assert_eq!(searches, vec![call]);
        assert_eq!(messages, vec![message]);
        assert_eq!(
            (&mut server.request).await.unwrap()["tool_choice"],
            json!({"type":"web_search"})
        );
        let server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[cited_message()]}}))],Duration::ZERO).await;
        assert!(search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Required,
            true,
            |_| {}
        )
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("no search call"));
        let server = mock_server(
            "200 OK",
            vec![
                frame(
                    json!({"type":"response.output_item.done","item":search_call("in_progress")}),
                ),
                completed(),
            ],
            Duration::ZERO,
        )
        .await;
        assert!(search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Auto,
            false,
            |_| {}
        )
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("incomplete output"));
    }
    #[tokio::test]
    async fn unrequested_search_is_rejected_at_every_stream_stage_and_bounded_calls_cannot_enable_it(
    ) {
        for events in [
            vec![frame(
                json!({"type":"response.output_item.added","item":search_call("in_progress")}),
            )],
            vec![frame(
                json!({"type":"response.output_item.done","item":search_call("completed")}),
            )],
            vec![frame(
                json!({"type":"response.completed","response":{"status":"completed","output":[search_call("completed")]}}),
            )],
            vec![frame(
                json!({"type":"response.web_search_call.searching","item_id":"ws_fixture","output_index":0}),
            )],
        ] {
            let server = mock_server("200 OK", events, Duration::ZERO).await;
            let mut seen = 0;
            let error = search_at(
                &mut test_client(),
                &server,
                crate::web_search::SearchMode::Disabled,
                false,
                |event| {
                    if matches!(event, StreamDelta::WebSearch(_)) {
                        seen += 1
                    }
                },
            )
            .await
            .err()
            .unwrap();
            assert!(error.to_string().contains("without a requested"));
            assert_eq!(seen, 0);
        }
        let server = mock_server("200 OK", vec![], Duration::ZERO).await;
        assert!(test_client()
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &json!([{"type":"web_search"}]),
                4096,
                77,
                |_| {}
            )
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("do not allow tools"));
        let server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[search_call("completed")]}}))],Duration::ZERO).await;
        assert!(test_client()
            .stream_turn_bounded(
                &server.base_url,
                "test",
                "auto",
                "",
                &[],
                &json!([]),
                4096,
                77,
                |_| {}
            )
            .await
            .is_err());
    }
    #[tokio::test]
    async fn unsafe_provider_links_and_search_error_states_never_become_fabricated_success() {
        let bad = json!({"type":"web_search_call","id":"ws_bad","status":"completed","action":{"type":"search","sources":[{"type":"url","url":"javascript:alert(1)"}]}});
        let server = mock_server(
            "200 OK",
            vec![frame(
                json!({"type":"response.output_item.done","item":bad}),
            )],
            Duration::ZERO,
        )
        .await;
        assert!(search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Live,
            false,
            |_| {}
        )
        .await
        .is_err());
        let failed = search_call("failed");
        let server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[failed.clone(),cited_message()]}}))],Duration::ZERO).await;
        let mut progress = Vec::new();
        let turn = search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Live,
            false,
            |event| {
                if let StreamDelta::WebSearch(item) = event {
                    progress.push(raw_snapshot(item).unwrap())
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(progress, vec![failed.clone()]);
        assert_eq!(turn.items[0], failed);
        assert!(turn.calls.is_empty());
    }

    #[tokio::test]
    async fn message_activation_tracks_unicode_interleaving_and_richer_final_citations_with_one_key(
    ) {
        let message = |id: &str, text: &str| json!({"type":"message","id":id,"role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]});
        let (a, b) = (message("a", "中文"), message("b", "🙂"));
        let mut rich = a.clone();
        rich["content"][0]["annotations"] = json!([{"type":"url_citation","url":"https://example.test","title":"来源","start_index":0,"end_index":2}]);
        let web = search_call("completed");
        let chunks = vec![
            frame(
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"a","role":"assistant","status":"in_progress","content":[]}}),
            ),
            frame(
                json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"b","role":"assistant","status":"in_progress","content":[]}}),
            ),
            frame(
                json!({"type":"response.output_text.delta","item_id":"a","output_index":0,"delta":"中"}),
            ),
            frame(
                json!({"type":"response.output_item.added","output_index":2,"item":search_call("in_progress")}),
            ),
            frame(
                json!({"type":"response.output_text.delta","item_id":"b","output_index":1,"delta":"🙂"}),
            ),
            frame(
                json!({"type":"response.output_item.added","output_index":3,"item":{"type":"reasoning","id":"r","summary":[]}}),
            ),
            frame(
                json!({"type":"response.output_text.delta","item_id":"a","output_index":0,"delta":"文"}),
            ),
            frame(json!({"type":"response.output_item.done","output_index":1,"item":b})),
            frame(json!({"type":"response.output_item.done","output_index":0,"item":a})),
            frame(json!({"type":"response.output_item.done","output_index":2,"item":web})),
            frame(
                json!({"type":"response.completed","response":{"status":"completed","output":[rich,b,web,{"type":"reasoning","id":"r","summary":[]}]}}),
            ),
        ];
        let server = mock_server("200 OK", chunks, Duration::ZERO).await;
        let (mut active, mut texts, mut snapshots) =
            (String::new(), HashMap::<String, String>::new(), Vec::new());
        let turn = search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Auto,
            false,
            |event| match event {
                StreamDelta::OutputMessage(item) => {
                    let key = item["_mcodex_item_key"].as_str().unwrap().to_string();
                    if item["_mcodex_activate"] == true {
                        active = key;
                        texts.entry(active.clone()).or_default();
                    } else {
                        let display = crate::web_search::project_message(item).unwrap();
                        texts.insert(key.clone(), display["text"].as_str().unwrap().into());
                        snapshots.push((key, item.clone()));
                    }
                }
                StreamDelta::Text(delta) => {
                    texts.entry(active.clone()).or_default().push_str(delta)
                }
                _ => {}
            },
        )
        .await
        .unwrap();
        assert_eq!(texts.len(), 2);
        let a_snapshots = snapshots
            .iter()
            .filter(|(_, item)| item["id"] == "a")
            .collect::<Vec<_>>();
        assert_eq!(a_snapshots.len(), 2);
        assert_eq!(a_snapshots[0].0, a_snapshots[1].0);
        assert_eq!(texts[&a_snapshots[0].0], "中文");
        assert_eq!(
            a_snapshots[1].1["content"][0]["annotations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let b_key = &snapshots
            .iter()
            .find(|(_, item)| item["id"] == "b")
            .unwrap()
            .0;
        assert_ne!(&a_snapshots[0].0, b_key);
        assert_eq!(texts[b_key], "🙂");
        assert!(!serde_json::to_string(&turn.items)
            .unwrap()
            .contains("_mcodex_"));
    }
    #[tokio::test]
    async fn identical_anonymous_messages_stay_distinct_and_final_enrichment_uses_the_original_slot(
    ) {
        let plain = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"same","annotations":[]}]});
        let mut rich = plain.clone();
        rich["content"][0]["annotations"] = json!([{"type":"url_citation","url":"https://example.test","title":"Source","start_index":0,"end_index":4}]);
        for prefixed in [false, true] {
            let mut chunks = Vec::new();
            if prefixed {
                for _ in 0..2 {
                    chunks.push(frame(
                        json!({"type":"response.output_item.done","item":plain}),
                    ));
                }
            }
            chunks.push(frame(json!({"type":"response.completed","response":{"status":"completed","output":[rich,plain]}})));
            let server = mock_server("200 OK", chunks, Duration::ZERO).await;
            let mut snapshots = Vec::new();
            let turn = search_at(
                &mut test_client(),
                &server,
                crate::web_search::SearchMode::Disabled,
                false,
                |event| {
                    if let StreamDelta::OutputMessage(item) = event {
                        if item["_mcodex_activate"] != true {
                            snapshots.push(item.clone());
                        }
                    }
                },
            )
            .await
            .unwrap();
            assert_eq!(turn.items.len(), 2);
            let keys = snapshots
                .iter()
                .map(|item| item["_mcodex_item_key"].as_str().unwrap())
                .collect::<HashSet<_>>();
            assert_eq!(keys.len(), 2);
            assert_eq!(snapshots.len(), if prefixed { 3 } else { 2 });
            if prefixed {
                assert_eq!(
                    snapshots[0]["_mcodex_item_key"],
                    snapshots[2]["_mcodex_item_key"]
                );
                assert_ne!(
                    snapshots[0]["_mcodex_item_key"],
                    snapshots[1]["_mcodex_item_key"]
                );
            }
            assert!(!serde_json::to_string(&turn.items)
                .unwrap()
                .contains("_mcodex_"));
        }
        let mut keys = Vec::new();
        for _ in 0..2 {
            let server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[search_call("completed")]}}))],Duration::ZERO).await;
            search_at(
                &mut test_client(),
                &server,
                crate::web_search::SearchMode::Auto,
                false,
                |event| {
                    if let StreamDelta::WebSearch(item) = event {
                        keys.push(item["_mcodex_item_key"].as_str().unwrap().to_string());
                    }
                },
            )
            .await
            .unwrap();
        }
        assert_ne!(
            keys[0], keys[1],
            "Different Responses reused a web activity UI identity"
        );
    }

    #[tokio::test]
    async fn provider_cannot_spoof_local_activation_or_presentation_control_fields() {
        let mut message = cited_message();
        message["_mcodex_activate"] = json!(true);
        message["_mcodex_item_key"] = json!("fake_provider_key");
        message["_mcodex_display"] = json!({"text":"forged visible content"});
        let mut call = search_call("completed");
        call["_mcodex_item_key"] = json!("fake_web_key");
        let server=mock_server("200 OK",vec![frame(json!({"type":"response.completed","response":{"status":"completed","output":[call,message]}}))],Duration::ZERO).await;
        let mut seen = Vec::new();
        let turn = search_at(
            &mut test_client(),
            &server,
            crate::web_search::SearchMode::Auto,
            false,
            |event| match event {
                StreamDelta::OutputMessage(item) | StreamDelta::WebSearch(item) => {
                    seen.push(item.clone())
                }
                _ => {}
            },
        )
        .await
        .unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen.iter().all(|item| item["_mcodex_activate"] != true
            && item.get("_mcodex_display").is_none()
            && item["_mcodex_item_key"]
                .as_str()
                .unwrap()
                .starts_with("response_")));
        assert!(!serde_json::to_string(&turn.items)
            .unwrap()
            .contains("_mcodex_"));
    }
}
