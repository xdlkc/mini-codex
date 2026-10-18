//! Streaming client for the Responses API (`POST {base}/responses`, SSE).
//! Works with both API-key auth (api.openai.com/v1) and ChatGPT auth (chatgpt.com/backend-api/codex).

use crate::auth::Auth;
use anyhow::{bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

pub struct Client {
    http: reqwest::Client,
    pub auth: Auth,
}

#[derive(Default)]
pub struct Turn {
    /// Every completed output item (messages, reasoning, function calls), to be replayed as history.
    pub items: Vec<Value>,
    pub calls: Vec<FunctionCall>,
}

pub struct FunctionCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

pub enum StreamDelta<'a> {
    Text(&'a str),
    ReasoningStart(&'a str),
    ReasoningDelta { id: &'a str, delta: &'a str },
    ReasoningDone { id: &'a str, text: String },
}

impl Client {
    pub fn new(auth: Auth) -> Self {
        Self { http: reqwest::Client::new(), auth }
    }

    pub async fn stream_turn(
        &mut self,
        model: &str,
        effort: &str,
        instructions: &str,
        input: &[Value],
        tools: &Value,
        mut on_delta: impl FnMut(StreamDelta<'_>),
    ) -> Result<Turn> {
        self.auth.ensure_fresh(&self.http).await?;
        let api_effort = match effort.to_lowercase().as_str() {
            "low" | "light" | "minimal" => "low",
            "high" | "xhigh" | "extra_high" | "ultra" | "max" | "persistent" => "high",
            _ => "medium",
        };
        // store=false everywhere: history is replayed client-side (required for ChatGPT auth).
        let body = json!({
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
        let mut req = self
            .http
            .post(format!("{}/responses", self.auth.base_url().trim_end_matches('/')))
            .header("originator", "codex_cli_rs")
            .header("accept", "text/event-stream");
        req = match &self.auth {
            Auth::ApiKey(k) => req.bearer_auth(k),
            Auth::ChatGpt { access, account_id, .. } => req
                .bearer_auth(access)
                .header("chatgpt-account-id", account_id)
                .header("OpenAI-Beta", "responses=experimental"),
        };
        let resp = req.json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!("API error {status}: {}", resp.text().await.unwrap_or_default());
        }

        let mut turn = Turn::default();
        let mut buf = String::new();
        let mut current_reasoning_id = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(idx) = buf.find("\n\n") {
                let event: String = buf.drain(..idx + 2).collect();
                let data: String = event
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(|d| d.trim_start())
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let v: Value = serde_json::from_str(&data)?;
                match v["type"].as_str().unwrap_or("") {
                    "response.output_item.added" => {
                        if v["item"]["type"] == "reasoning" {
                            if let Some(id) = v["item"]["id"].as_str() {
                                current_reasoning_id = id.to_string();
                                on_delta(StreamDelta::ReasoningStart(id));
                            }
                        }
                    }
                    "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                        if let Some(d) = v["delta"].as_str() {
                            let id = v["item_id"].as_str().unwrap_or(&current_reasoning_id);
                            on_delta(StreamDelta::ReasoningDelta { id, delta: d });
                        }
                    }
                    "response.reasoning_summary_part.added" => {
                        let id = v["item_id"].as_str().unwrap_or(&current_reasoning_id);
                        on_delta(StreamDelta::ReasoningDelta { id, delta: "\n" });
                    }
                    "response.output_text.delta" => {
                        if let Some(d) = v["delta"].as_str() {
                            on_delta(StreamDelta::Text(d));
                        }
                    }
                    "response.output_item.done" => {
                        let item = v["item"].clone();
                        if item["type"] == "reasoning" {
                            let id = item["id"].as_str().unwrap_or(&current_reasoning_id);
                            let summary: String = item["summary"]
                                .as_array()
                                .map(|arr| arr.iter().filter_map(|s| s["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
                                .unwrap_or_default();
                            on_delta(StreamDelta::ReasoningDone { id, text: summary });
                        } else if item["type"] == "function_call" {
                            turn.calls.push(FunctionCall {
                                call_id: item["call_id"].as_str().unwrap_or_default().into(),
                                name: item["name"].as_str().unwrap_or_default().into(),
                                arguments: item["arguments"].as_str().unwrap_or("{}").into(),
                            });
                        }
                        turn.items.push(item);
                    }
                    "response.failed" | "error" => bail!("stream error: {v}"),
                    _ => {}
                }
            }
        }
        Ok(turn)
    }
}
