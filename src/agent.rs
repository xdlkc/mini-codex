//! The agent loop: model turn -> tool calls -> results -> next turn, emitting UI events.

use crate::api::{Client, StreamDelta};
use crate::session::Session;
use crate::tools::{self, ToolCtx};
use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

const INTERNAL_MARKER: &str = "_mcodex_internal";

pub(crate) fn is_internal_user(item: &Value) -> bool {
    item["type"] == "message" && item["role"] == "user" && item[INTERNAL_MARKER] == true
}

fn strip_private_fields(item: &mut Value) {
    if let Some(fields) = item.as_object_mut() {
        fields.retain(|key, _| !key.starts_with("_mcodex_"));
    }
}

pub(crate) fn model_history(mut history: Vec<Value>) -> Vec<Value> {
    for item in &mut history {
        strip_private_fields(item);
    }
    history
}

fn user_record(content: Vec<Value>, internal: bool) -> Value {
    let mut item = json!({"type":"message","role":"user","content":content});
    if internal {
        item[INTERNAL_MARKER] = json!(true);
    }
    item
}

/// Answer to an approval request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Yes,
    /// Approve and stop asking for the rest of the session.
    Always,
    No,
    Cancel,
    SaveRule,
}

pub enum Event {
    Text(String),
    WebSearch(Value),
    AssistantMessage(Value),
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        delta: String,
    },
    ReasoningEnd {
        id: String,
        text: String,
    },
    ToolStart {
        id: String,
        name: String,
        args: String,
    },
    ToolEnd {
        id: String,
        output: String,
        denied: bool,
    },
    ToolExecuting {
        id: String,
    },
    Approval {
        id: String,
        name: String,
        what: String,
        cwd: String,
        reason: Option<String>,
        allow_session: bool,
        files: Vec<String>,
        proposed_rule: Option<Vec<String>>,
        reply: oneshot::Sender<Verdict>,
    },
    Usage {
        tokens: Option<u64>,
    },
    GoalState(Value),
    TaskReport(Value),
    ApprovalReview(Value),
    TurnCancelled,
    Warning(String),
    Done,
    Error(String),
}

pub struct Agent {
    pub approvals_reviewer: crate::guardian::Reviewer,
    pub model_catalog: std::sync::Arc<crate::models::Catalog>,
    pub pending_context: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    pub client: Client,
    pub model: String,
    pub reasoning_effort: String,
    pub mode: String,
    pub web_search: crate::web_search::SearchMode,
    pub goal_thread_id: Option<String>,
    pub scheduled_run_id: Option<String>,
    pub instructions: String,
    pub ctx: ToolCtx,
    pub history: Vec<Value>,
    pub session: Session,
}

impl Agent {
    pub fn drain_context(&mut self) -> Result<()> {
        let mut pending = self.pending_context.lock().unwrap();
        if pending.is_empty() {
            return Ok(());
        }
        self.session.append_batch_checked(&pending)?;
        self.history.extend(pending.drain(..));
        Ok(())
    }
    fn record(&mut self, item: Value) {
        self.session.append(&item);
        self.history.push(item);
    }

    pub fn reset(&mut self, cwd: &str) -> Result<()> {
        self.history.clear();
        self.session = Session::create(cwd, &self.model)?;
        Ok(())
    }

    /// After an interrupted turn, give every unanswered function call an output so the
    /// history stays valid for the API.
    pub fn fix_dangling(&mut self) {
        let answered: std::collections::HashSet<String> = self
            .history
            .iter()
            .filter(|i| i["type"] == "function_call_output")
            .filter_map(|i| i["call_id"].as_str().map(String::from))
            .collect();
        let missing: Vec<String> = self
            .history
            .iter()
            .filter(|i| i["type"] == "function_call")
            .filter_map(|i| i["call_id"].as_str().map(String::from))
            .filter(|c| !answered.contains(c))
            .collect();
        for call_id in missing {
            self.record(json!({"type": "function_call_output", "call_id": call_id, "output": "aborted by user"}));
        }
    }

    /// Runs one user turn to completion. Always ends by sending `Done` or `Error`.
    pub async fn turn(&mut self, user: &str, tx: &UnboundedSender<Event>) {
        self.turn_content(vec![json!({"type":"input_text", "text":user})], tx)
            .await;
    }

    pub async fn turn_content(&mut self, content: Vec<Value>, tx: &UnboundedSender<Event>) {
        self.turn_content_with_visibility(content, tx, false).await;
    }

    pub async fn turn_content_with_visibility(
        &mut self,
        content: Vec<Value>,
        tx: &UnboundedSender<Event>,
        internal: bool,
    ) {
        self.turn_content_presented(content, tx, internal, None)
            .await;
    }

    pub async fn turn_content_presented(
        &mut self,
        content: Vec<Value>,
        tx: &UnboundedSender<Event>,
        internal: bool,
        presentation: Option<Value>,
    ) {
        // Retain host provenance for the reviewer. Responses receives a clean
        // projection, including when this history came from a saved rollout.
        self.fix_dangling();
        match self.turn_inner(content, tx, internal, presentation).await {
            Ok(()) => tx.send(Event::Done).ok(),
            Err(e) => tx.send(Event::Error(format!("{e:#}"))).ok(),
        };
    }

    async fn turn_inner(
        &mut self,
        content: Vec<Value>,
        tx: &UnboundedSender<Event>,
        internal: bool,
        presentation: Option<Value>,
    ) -> Result<()> {
        let mut record = user_record(content, internal);
        if let Some(presentation) = presentation {
            record["_mcodex_display"] = presentation;
        }
        self.record(record);
        let mut first_request = true;
        loop {
            self.drain_context()?;
            // A running model step retains its issuing permissions. Updates are
            // picked up before the next step, never halfway through a review.
            if let Some(saved) = crate::desktop::handle(
                "desktop/threadMetadata/read",
                &json!({"threadId":self.session.id}),
            ) {
                let saved = saved.map_err(anyhow::Error::msg)?;
                let saved = &saved["metadata"];
                if let Some(mode) = saved["sandboxMode"].as_str() {
                    self.ctx.sandbox = match mode {
                        "read-only" => tools::Sandbox::ReadOnly,
                        "danger-full-access" => tools::Sandbox::DangerFullAccess,
                        _ => tools::Sandbox::WorkspaceWrite,
                    };
                }
                if let Some(policy) = saved["approvalPolicy"].as_str() {
                    self.ctx.auto_approve = policy == "never";
                }
                if let Some(reviewer) = saved.get("approvalsReviewer") {
                    self.approvals_reviewer = crate::guardian::Reviewer::parse(reviewer)?;
                }
            }
            let mut tools = if self.mode == "chat" {
                json!([])
            } else {
                tools::definitions(&self.ctx)
            };
            if let Some(tool) = self.web_search.tool() {
                tools.as_array_mut().unwrap().push(tool);
            }
            let mut instructions = self.instructions.clone();
            if let Some(thread_id) = &self.goal_thread_id {
                let goal = crate::goals::get(thread_id)
                    .map_err(anyhow::Error::msg)?
                    .ok_or_else(|| anyhow::anyhow!("Goal was cleared"))?;
                if goal["status"] != "active" {
                    anyhow::bail!(
                        "Goal is {}",
                        goal["status"].as_str().unwrap_or("unavailable")
                    );
                }
                instructions.push_str(&format!("\n\n# Active goal\n{}\nKeep working toward this entire objective. Verify completion against concrete evidence. Do not redefine success around partial work. Before ending this turn, call goal_checkpoint with evidence, remaining work, and either active, complete, or blocked. Complete requires the entire objective to be achieved and verified, with no remaining required work. A budget limit or an error is not completion. For a blocker, provide a stable blockerKey describing the same root cause across turns. Do not claim progress without evidence. Do not widen permissions or skip approvals. User messages steer the existing goal.\n",goal["objective"].as_str().unwrap_or("")));
                tools.as_array_mut().unwrap().push(json!({"type":"function","name":"goal_checkpoint","description":"Record verified progress toward the active goal and end this turn. Use complete only when the entire objective is proven complete.","parameters":{"type":"object","properties":{"status":{"type":"string","enum":["active","complete","blocked"]},"summary":{"type":"string"},"evidence":{"type":"array","items":{"type":"string"}},"remaining":{"type":"array","items":{"type":"string"}},"nextPrompt":{"type":"string"},"blockerKey":{"type":"string"}},"required":["status","summary","evidence","remaining"],"additionalProperties":false}}));
            }
            if self.scheduled_run_id.is_some() {
                instructions.push_str("\n\nThis is a scheduled task run. Finish the saved task, then call scheduled_task_report with a concise summary and hasUpdate. Set hasUpdate=false only when the run found no meaningful change or result to report. Do not claim successful actions without tool evidence. This does not change sandbox or approval requirements.");
                tools.as_array_mut().unwrap().push(json!({"type":"function","name":"scheduled_task_report","description":"Record the outcome of this scheduled run and end this turn.","parameters":{"type":"object","properties":{"summary":{"type":"string"},"hasUpdate":{"type":"boolean"}},"required":["summary","hasUpdate"],"additionalProperties":false}}));
            }
            let turn = self
                .client
                .stream_turn_search(
                    &self.model,
                    &self.reasoning_effort,
                    &instructions,
                    &model_history(self.history.clone()),
                    &tools,
                    first_request && self.web_search == crate::web_search::SearchMode::Required,
                    |ev| match ev {
                        StreamDelta::Text(t) => {
                            let _ = tx.send(Event::Text(t.to_string()));
                        }
                        StreamDelta::WebSearch(item) => {
                            let _ = tx.send(Event::WebSearch(item.clone()));
                        }
                        StreamDelta::OutputMessage(item) => {
                            let _ = tx.send(Event::AssistantMessage(item.clone()));
                        }
                        StreamDelta::ReasoningStart(id) => {
                            let _ = tx.send(Event::ReasoningStart { id: id.to_string() });
                        }
                        StreamDelta::ReasoningDelta { id, delta } => {
                            let _ = tx.send(Event::ReasoningDelta {
                                id: id.to_string(),
                                delta: delta.to_string(),
                            });
                        }
                        StreamDelta::ReasoningDone { id, text } => {
                            let _ = tx.send(Event::ReasoningEnd {
                                id: id.to_string(),
                                text,
                            });
                        }
                    },
                )
                .await?;
            first_request = false;
            let _ = tx.send(Event::Usage {
                tokens: turn.tokens_used,
            });
            if let Some(thread_id) = &self.goal_thread_id {
                let state = match turn.tokens_used {
                    Some(tokens) => crate::goals::add_usage(thread_id, tokens),
                    None => crate::goals::usage_unavailable(thread_id),
                }
                .map_err(anyhow::Error::msg)?;
                if let Some(state) = state {
                    let stopped = state["status"] != "active";
                    let _ = tx.send(Event::GoalState(state));
                    if stopped {
                        for item in turn
                            .items
                            .into_iter()
                            .filter(|i| i["type"] != "function_call")
                        {
                            self.record(item);
                        }
                        anyhow::bail!("Goal stopped at its usage or token budget limit");
                    }
                }
            }
            if self.mode == "chat"
                && turn.calls.iter().any(|call| {
                    !["goal_checkpoint", "scheduled_task_report"].contains(&call.name.as_str())
                })
            {
                anyhow::bail!("The model requested a tool that is unavailable in Chat mode");
            }
            for call in &turn.calls {
                if !tools.as_array().is_some_and(|list| {
                    list.iter()
                        .any(|tool| tool["name"].as_str() == Some(call.name.as_str()))
                }) {
                    anyhow::bail!("The model requested an unavailable tool: {}", call.name);
                }
            }
            let has_assistant_text = turn.items.iter().any(|item| {
                item["type"] == "message"
                    && item["role"] == "assistant"
                    && item["content"].as_array().is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|p| p["text"].as_str().is_some_and(|t| !t.trim().is_empty()))
                    })
            });
            let mixed_control = turn.calls.len() > 1;
            for item in turn.items {
                self.record(item);
            }
            if turn.calls.is_empty() {
                return Ok(());
            }
            for c in turn.calls {
                if ["goal_checkpoint", "scheduled_task_report"].contains(&c.name.as_str())
                    && mixed_control
                {
                    self.record(json!({"type":"function_call_output","call_id":c.call_id,"output":"Call this control tool by itself after completing the other tools."}));
                    continue;
                }
                if c.name == "goal_checkpoint" {
                    let thread_id = self
                        .goal_thread_id
                        .as_deref()
                        .ok_or_else(|| anyhow::anyhow!("No active goal"))?;
                    let report: Value = serde_json::from_str(&c.arguments)?;
                    match crate::goals::checkpoint(thread_id, &report) {
                        Ok(state) => {
                            self.record(json!({"type":"function_call_output","call_id":c.call_id,"output":"Goal checkpoint saved"}));
                            if !has_assistant_text {
                                self.report_summary(&report, tx);
                            }
                            let _ = tx.send(Event::GoalState(state));
                            return Ok(());
                        }
                        Err(error) => {
                            self.record(json!({"type":"function_call_output","call_id":c.call_id,"output":format!("Invalid checkpoint: {error}")}));
                            continue;
                        }
                    }
                }
                if c.name == "scheduled_task_report" {
                    let report: Value = serde_json::from_str(&c.arguments)?;
                    if report["summary"].as_str().is_none()
                        || report["hasUpdate"].as_bool().is_none()
                    {
                        anyhow::bail!("Invalid scheduled task report");
                    }
                    self.record(json!({"type":"function_call_output","call_id":c.call_id,"output":"Scheduled run report saved"}));
                    if !has_assistant_text {
                        self.report_summary(&report, tx);
                    }
                    let _ = tx.send(Event::TaskReport(report));
                    return Ok(());
                }
                let _ = tx.send(Event::ToolStart {
                    id: c.call_id.clone(),
                    name: c.name.clone(),
                    args: c.arguments.clone(),
                });
                let plan = tools::prepare(&c.name, &c.arguments, &self.ctx);
                let out = if let Ok(mut plan) = plan {
                    plan.report_warnings(tx);
                    if plan.needs_review()
                        && self.approvals_reviewer == crate::guardian::Reviewer::Guardian
                    {
                        let review_model = self
                            .model_catalog
                            .review_model(&self.model, &self.reasoning_effort)
                            .await;
                        self.drain_context()?;
                        let assessment = crate::guardian::review(
                            &self.client,
                            &review_model,
                            &self.instructions,
                            &self.history,
                            &self.ctx,
                            &c.call_id,
                            &plan,
                            self.goal_thread_id.as_deref(),
                            tx,
                        )
                        .await;
                        if assessment.approved {
                            tools::run_plan(&c.call_id, plan, &self.ctx, tx, true).await
                        } else {
                            tools::ToolResult::denied(format!("Automatic approval review denied this action: {}. Explain the blocked action and its reason to the user. Do not claim it ran. If authorization is missing, ask for that specific authorization. If review failed, address the reported failure before retrying. Do not bypass this decision by changing tools or command spelling.",assessment.review["rationale"].as_str().unwrap_or("The action was not approved")))
                        }
                    } else {
                        tools::run_plan(&c.call_id, plan, &self.ctx, tx, false).await
                    }
                } else {
                    tools::run(&c.call_id, &c.name, &c.arguments, &self.ctx, tx).await
                };
                let _ = tx.send(Event::ToolEnd {
                    id: c.call_id.clone(),
                    output: out.output.clone(),
                    denied: out.denied,
                });
                self.record(
                    json!({"type": "function_call_output", "call_id": c.call_id, "output": out.output,"_mcodex_tool_denied":out.denied}),
                );
                if out.cancelled {
                    let _ = tx.send(Event::TurnCancelled);
                    return Ok(());
                }
            }
        }
    }
    fn report_summary(&mut self, report: &Value, tx: &UnboundedSender<Event>) {
        if let Some(summary) = report["summary"].as_str().filter(|s| !s.trim().is_empty()) {
            self.record(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":summary}]}));
            let _ = tx.send(Event::Text(summary.to_string()));
        }
    }
}

/// Plain-text rendering of history (used when resuming).
pub fn render_history(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .filter(|item| !is_internal_user(item))
        .filter_map(|i| match (i["type"].as_str(), i["role"].as_str()) {
            (Some("message"), Some(role)) => {
                let mut text: String = i["_mcodex_display"]["text"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| {
                        i["content"]
                            .as_array()
                            .map(|parts| parts.iter().filter_map(|c| c["text"].as_str()).collect())
                            .unwrap_or_default()
                    });
                if let Some(files) = i["_mcodex_display"]["attachments"].as_array() {
                    for file in files {
                        if let Some(name) = file["name"].as_str() {
                            text.push_str(&format!("\n[Attached: {name}]"));
                        }
                    }
                }
                Some((role.to_string(), text))
            }
            (Some("function_call"), _) => Some((
                "tool".into(),
                format!("{} {}", i["name"].as_str()?, i["arguments"].as_str()?),
            )),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_visibility_is_persisted_separately_from_model_input() {
        let content = vec![
            json!({"type":"input_text","text":"Continue the full objective; keep the literal _mcodex_internal text."}),
        ];
        let stored = user_record(content.clone(), true);
        assert!(is_internal_user(&stored));
        let mut public = stored.clone();
        // Agent::record uses this exact transformation after writing the rollout.
        strip_private_fields(&mut public);
        assert!(public.get(INTERNAL_MARKER).is_none());
        assert_eq!(public["content"], json!(content));
        assert_eq!(stored[INTERNAL_MARKER], true);
        let restored = model_history(vec![
            stored.clone(),
            json!({"type":"message","role":"user","_mcodex_internal":false,"content":[]}),
        ]);
        assert!(restored
            .iter()
            .all(|item| item.get(INTERNAL_MARKER).is_none()));
        assert_eq!(restored[0], public);
        assert!(stored.get(INTERNAL_MARKER).is_some());
        assert!(!is_internal_user(&user_record(vec![], false)));
    }

    #[test]
    fn tui_history_hides_internal_prompts_but_preserves_assistant_and_human_messages() {
        let text = |value: &str| vec![json!({"type":"input_text","text":value})];
        let items = vec![
            user_record(text("Human objective"), false),
            json!({"type":"message","role":"assistant","content":[{"text":"Initial result"}]}),
            user_record(text("Internal continuation"), true),
            json!({"type":"message","role":"assistant","content":[{"text":"Continued result"}]}),
            user_record(text("Human follow-up"), false),
        ];
        let rendered = render_history(&items);
        assert_eq!(
            rendered
                .iter()
                .map(|(_, text)| text.as_str())
                .collect::<Vec<_>>(),
            vec![
                "Human objective",
                "Initial result",
                "Continued result",
                "Human follow-up"
            ]
        );
    }

    #[test]
    fn attachment_presentation_and_voice_markers_never_enter_model_history() {
        let original = json!({"type":"message","role":"user","content":[{"type":"input_text","text":"Complete extracted document"}],"_mcodex_display":{"text":"Read this","attachments":[{"id":"att_test","name":"report.pdf"}]},"_mcodex_voice":{"id":"voice_test"}});
        let public = model_history(vec![original.clone()]);
        assert_eq!(public[0]["content"], original["content"]);
        assert!(public[0].get("_mcodex_display").is_none());
        assert!(public[0].get("_mcodex_voice").is_none());
        assert_eq!(
            render_history(&[original]),
            vec![("user".into(), "Read this\n[Attached: report.pdf]".into())]
        );
    }

    struct Loopback {
        base: String,
        requests: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
        job: tokio::task::JoinHandle<()>,
    }
    impl Drop for Loopback {
        fn drop(&mut self) {
            self.job.abort();
        }
    }
    async fn loopback(outputs: Vec<Vec<Value>>) -> Loopback {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = requests.clone();
        let job = tokio::spawn(async move {
            for output in outputs {
                let (mut socket, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let size = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        socket.read(&mut buffer),
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert!(size > 0);
                    bytes.extend_from_slice(&buffer[..size]);
                    assert!(bytes.len() < 8 * 1024 * 1024);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        assert!(header
                            .to_ascii_lowercase()
                            .contains("authorization: bearer isolated-test-key"));
                        let size = header
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                            .unwrap()
                            .1
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + size {
                            observed.lock().unwrap().push(
                                serde_json::from_slice(&bytes[end + 4..end + 4 + size]).unwrap(),
                            );
                            break;
                        }
                    }
                }
                let body = format!(
                    "data: {}\r\n\r\n",
                    json!({"type":"response.completed","response":{"status":"completed","output":output,"usage":{"total_tokens":5}}})
                );
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        Loopback {
            base,
            requests,
            job,
        }
    }
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mcodex-agent-search-{:032x}",
                rand::random::<u128>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn isolated(
        root: &Temp,
        base: &str,
        mode: &str,
        search: crate::web_search::SearchMode,
    ) -> Agent {
        Agent {
            approvals_reviewer: Default::default(),
            model_catalog: std::sync::Arc::new(crate::models::Catalog::default()),
            pending_context: Default::default(),
            client: Client::test_at(base),
            model: "local-fixture".into(),
            reasoning_effort: "auto".into(),
            mode: mode.into(),
            web_search: search,
            goal_thread_id: None,
            scheduled_run_id: None,
            instructions: "Fixture".into(),
            ctx: ToolCtx {
                cwd: root.0.clone(),
                sandbox: crate::tools::Sandbox::ReadOnly,
                auto_approve: false,
                mcp: std::sync::Arc::new(crate::mcp::Mcp::default()),
                plugins: vec![],
                session_allow: std::sync::atomic::AtomicBool::new(false),
                patch_grants: Default::default(),
                rules_home: None,
            },
            history: vec![],
            session: Session::create_in(&root.0, &root.0.to_string_lossy(), "local-fixture")
                .unwrap(),
        }
    }
    fn answer(text: &str) -> Value {
        json!({"type":"message","id":"same_provider_id","role":"assistant","content":[{"type":"output_text","text":text,"annotations":[]}]})
    }
    fn searched() -> Value {
        json!({"type":"web_search_call","id":"ws_real","status":"completed","action":{"type":"search","queries":["fixture"],"sources":[{"type":"url","url":"https://example.test/source"}]}})
    }
    fn events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Vec<Event> {
        let mut result = Vec::new();
        while let Ok(event) = rx.try_recv() {
            result.push(event);
        }
        result
    }
    #[tokio::test]
    async fn hosted_search_chat_has_no_local_tools_and_history_replays_raw_citations_without_ui_keys(
    ) {
        let root = Temp::new();
        let mut message = answer("中文🙂 cited");
        message["content"][0]["annotations"] = json!([{"type":"url_citation","url":"https://example.test/source","title":"Source","start_index":0,"end_index":4}]);
        let server = loopback(vec![
            vec![searched(), message.clone()],
            vec![answer("A normal follow-up")],
        ])
        .await;
        let mut agent = isolated(
            &root,
            &server.base,
            "chat",
            crate::web_search::SearchMode::Cached,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        agent.turn("Search this fixture", &tx).await;
        let first = events(&mut rx);
        assert!(first.iter().any(|event| matches!(event, Event::Done)));
        assert!(first
            .iter()
            .any(|event| matches!(event,Event::WebSearch(item) if item["status"]=="completed")));
        assert!(first.iter().any(|event|matches!(event,Event::AssistantMessage(item) if item["content"]==message["content"]&&item["_mcodex_item_key"].is_string())));
        assert!(!first.iter().any(|event| matches!(
            event,
            Event::ToolStart { .. } | Event::Approval { .. } | Event::Error(_)
        )));
        assert_eq!(agent.history[1], searched());
        assert_eq!(agent.history[2], message);
        assert!(!serde_json::to_string(&agent.history)
            .unwrap()
            .contains("_mcodex_item_key"));
        let rollout = std::fs::read_to_string(
            root.0
                .join("sessions")
                .join(format!("{}.jsonl", agent.session.id)),
        )
        .unwrap();
        assert!(rollout.contains("web_search_call") && rollout.contains("url_citation"));
        assert!(!rollout.contains("_mcodex_item_key"));
        agent.web_search = crate::web_search::SearchMode::Disabled;
        agent.turn("Use the existing context", &tx).await;
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests[0]["tools"],
            json!([{"type":"web_search","external_web_access":false}])
        );
        assert_eq!(requests[1]["tools"], json!([]));
        assert!(requests[1]["input"].as_array().unwrap().contains(&message));
        assert!(!requests[1].to_string().contains("_mcodex_"));
        let last = events(&mut rx);
        assert!(!last
            .iter()
            .any(|event| matches!(event, Event::WebSearch(_) | Event::Error(_))));
        let keys = first
            .iter()
            .chain(last.iter())
            .filter_map(|event| match event {
                Event::AssistantMessage(item) => item["_mcodex_item_key"].as_str(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(keys.len(), 2);
        assert_ne!(
            keys[0], keys[1],
            "Responses with reused provider ids shared a UI key"
        );
    }
    #[tokio::test]
    async fn required_search_forces_only_first_request_and_work_keeps_declared_local_tools() {
        let root = Temp::new();
        let call = json!({"type":"function_call","id":"fc_1","call_id":"apply_1","name":"apply_patch","arguments":json!({"input":"*** Begin Patch\n*** Add File: forbidden\n+bad\n*** End Patch"}).to_string()});
        let server = loopback(vec![
            vec![searched(), call],
            vec![answer("Read-only boundary preserved")],
        ])
        .await;
        let mut agent = isolated(
            &root,
            &server.base,
            "work",
            crate::web_search::SearchMode::Required,
        );
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let work = agent.turn("Search before continuing", &tx);
        tokio::pin!(work);
        let mut denied_patch = false;
        let mut observed = Vec::new();
        loop {
            tokio::select! {
                _=&mut work=>break,
                event=rx.recv()=>if let Some(event)=event {match event{Event::Approval{name,reply,..}=>{assert_eq!(name,"apply_patch");denied_patch=true;reply.send(Verdict::No).unwrap();},event=>observed.push(event)}},
            }
        }
        drop(work);
        assert!(denied_patch);
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["tool_choice"], json!({"type":"web_search"}));
        assert_eq!(requests[1]["tool_choice"], "auto");
        for request in requests.iter() {
            assert!(request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "web_search"));
            assert!(request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "function" && tool["name"] == "apply_patch"));
        }
        observed.extend(events(&mut rx));
        assert!(observed
            .iter()
            .any(|event| matches!(event, Event::ToolEnd { denied: true, .. })));
        assert!(observed.iter().any(|event| matches!(event, Event::Done)));
        assert!(!observed
            .iter()
            .any(|event| matches!(event, Event::Error(_) | Event::Approval { .. })));
        assert!(!root.0.join("forbidden").exists());
    }
    #[tokio::test]
    async fn chat_rejects_provider_local_function_calls_even_when_search_is_enabled() {
        for name in ["shell", "web_search"] {
            let root = Temp::new();
            let call = json!({"type":"function_call","id":"fc_bad","call_id":"bad","name":name,"arguments":json!({"command":["touch",root.0.join("forbidden")]}).to_string()});
            let server = loopback(vec![vec![call]]).await;
            let mut agent = isolated(
                &root,
                &server.base,
                "chat",
                crate::web_search::SearchMode::Auto,
            );
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            agent.turn("An ordinary search prompt", &tx).await;
            let events = events(&mut rx);
            assert!(events.iter().any(
                |event| matches!(event,Event::Error(error) if error.contains("unavailable in Chat"))
            ));
            assert!(!events.iter().any(|event| matches!(
                event,
                Event::ToolStart { .. } | Event::Approval { .. } | Event::Done
            )));
            assert_eq!(agent.history.len(), 1);
            assert!(!root.0.join("forbidden").exists());
            assert_eq!(
                server.requests.lock().unwrap()[0]["tools"],
                json!([{"type":"web_search"}])
            );
        }
    }
}
