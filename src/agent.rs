//! The agent loop: model turn -> tool calls -> results -> next turn, emitting UI events.

use crate::api::{Client, StreamDelta};
use crate::session::Session;
use crate::tools::{self, ToolCtx};
use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::{mpsc::UnboundedSender, oneshot};

/// Answer to an approval request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Yes,
    /// Approve and stop asking for the rest of the session.
    Always,
    No,
}

pub enum Event {
    Text(String),
    ReasoningStart { id: String },
    ReasoningDelta { id: String, delta: String },
    ReasoningEnd { id: String, text: String },
    ToolStart { id: String, name: String, args: String },
    ToolEnd { id: String, output: String },
    Approval { id: String, name: String, what: String, reply: oneshot::Sender<Verdict> },
    Done,
    Error(String),
}

pub struct Agent {
    pub client: Client,
    pub model: String,
    pub reasoning_effort: String,
    pub instructions: String,
    pub ctx: ToolCtx,
    pub history: Vec<Value>,
    pub session: Session,
}

impl Agent {
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
        self.fix_dangling();
        match self.turn_inner(user, tx).await {
            Ok(()) => tx.send(Event::Done).ok(),
            Err(e) => tx.send(Event::Error(format!("{e:#}"))).ok(),
        };
    }

    async fn turn_inner(&mut self, user: &str, tx: &UnboundedSender<Event>) -> Result<()> {
        self.record(json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": user}]}));
        loop {
            let tools = tools::definitions(&self.ctx);
            let turn = self
                .client
                .stream_turn(&self.model, &self.reasoning_effort, &self.instructions, &self.history, &tools, |ev| match ev {
                    StreamDelta::Text(t) => {
                        let _ = tx.send(Event::Text(t.to_string()));
                    }
                    StreamDelta::ReasoningStart(id) => {
                        let _ = tx.send(Event::ReasoningStart { id: id.to_string() });
                    }
                    StreamDelta::ReasoningDelta { id, delta } => {
                        let _ = tx.send(Event::ReasoningDelta { id: id.to_string(), delta: delta.to_string() });
                    }
                    StreamDelta::ReasoningDone { id, text } => {
                        let _ = tx.send(Event::ReasoningEnd { id: id.to_string(), text });
                    }
                })
                .await?;
            for item in turn.items {
                self.record(item);
            }
            if turn.calls.is_empty() {
                return Ok(());
            }
            for c in turn.calls {
                let _ = tx.send(Event::ToolStart { id: c.call_id.clone(), name: c.name.clone(), args: c.arguments.clone() });
                let out = tools::run(&c.call_id, &c.name, &c.arguments, &self.ctx, tx).await;
                let _ = tx.send(Event::ToolEnd { id: c.call_id.clone(), output: out.clone() });
                self.record(json!({"type": "function_call_output", "call_id": c.call_id, "output": out}));
            }
        }
    }
}

/// Plain-text rendering of history (used when resuming).
pub fn render_history(items: &[Value]) -> Vec<(String, String)> {
    items
        .iter()
        .filter_map(|i| match (i["type"].as_str(), i["role"].as_str()) {
            (Some("message"), Some(role)) => {
                let text: String = i["content"].as_array()?.iter().filter_map(|c| c["text"].as_str()).collect();
                Some((role.to_string(), text))
            }
            (Some("function_call"), _) => Some(("tool".into(), format!("{} {}", i["name"].as_str()?, i["arguments"].as_str()?))),
            _ => None,
        })
        .collect()
}
