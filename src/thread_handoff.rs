//! Coordinates Git migration with a new conversation and archive of its source.
//! Goals stay on the original thread. Archiving removes its heartbeat definitions.
use super::*;

struct HandoffActivity {
    server: std::sync::Weak<Server>,
    operation: String,
}
impl Drop for HandoffActivity {
    fn drop(&mut self) {
        if let Some(server) = self.server.upgrade() {
            server
                .active_handoffs
                .lock()
                .unwrap()
                .remove(&self.operation);
        }
    }
}

impl Server {
    pub(super) async fn handoff_thread(self: &Arc<Self>, p: Value) -> RpcResult {
        let requested = p["threadId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or((-32602, "threadId is required".into()))?;
        let operation = match p.get("operationId") {
            Some(value) => value
                .as_str()
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 128
                        && id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
                })
                .ok_or((-32602, "Invalid operationId".into()))?
                .to_string(),
            None => self.new_id("handoff"),
        };
        {
            let mut active = self.active_handoffs.lock().unwrap();
            if active.contains_key(&operation) {
                return Err((-32000, "This handoff request is already running".into()));
            }
            active.insert(operation.clone(), requested.to_string());
        }
        let _activity = HandoffActivity {
            server: Arc::downgrade(self),
            operation: operation.clone(),
        };
        let result = self.handoff_thread_inner(p.clone(), &operation).await;
        if let Err(error) = &result {
            self.notify("thread/handoff/progress",json!({"threadId":requested,"operationId":operation,"stage":"error","message":error.1}));
        }
        result
    }

    async fn handoff_thread_inner(self: &Arc<Self>, p: Value, operation: &str) -> RpcResult {
        let _gate = self.workspace_gate.lock().await;
        let bad = |message: &str| (-32602, message.to_string());
        let requested = p["threadId"]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| bad("threadId is required"))?;
        if !["toWorktree", "toLocal"]
            .iter()
            .any(|direction| p["direction"] == *direction)
        {
            return Err(bad("direction must be toWorktree or toLocal"));
        }
        for key in ["interrupt", "continueExisting"] {
            if p.get(key).is_some_and(|value| !value.is_boolean()) {
                return Err(bad(&format!("{key} must be a boolean")));
            }
        }
        let (source, _, meta) =
            Session::open(Some(requested)).map_err(|e| (-32000, e.to_string()))?;
        let id = source.id.clone();
        let mut reservation = thread_lifecycle::exclusive(&id).map_err(|e| (-32000, e))?;
        if source.is_read_only() {
            return Err(bad("Fork an imported chat before handing it off"));
        }
        let metadata = desktop::handle("desktop/threadMetadata/read", &json!({"threadId":id}))
            .unwrap()
            .map_err(|e| (-32000, e))?["metadata"]
            .clone();
        if metadata["mode"] != "codex" || metadata["archived"] == true {
            return Err(bad("A handoff requires an unarchived Codex chat"));
        }
        let loaded = self.threads.lock().unwrap().get(&id).cloned();
        let draining = loaded.as_ref().is_some_and(|thread| thread.is_active());
        let _start = match loaded.as_ref() {
            Some(thread) => Some(thread.start_lock.lock().await),
            None => None,
        };
        if let Some(thread) = loaded.as_ref() {
            if !thread.queued_inputs.lock().unwrap().is_empty() {
                return Err((
                    -32000,
                    "Wait for queued messages before handing off this chat".into(),
                ));
            }
            if thread.voice_call.lock().unwrap().is_some() {
                return Err((
                    -32000,
                    "End the voice call before handing off this chat".into(),
                ));
            }
            if thread.is_active() {
                if p["continueExisting"] == true {
                    return Err((-32000,"Wait for the current response to finish before moving this chat into an existing worktree without changing its branch".into()));
                }
                if p["interrupt"] != true {
                    return Err((-32000,"This chat has a running response; handoff requires interrupt: true or waiting for completion".into()));
                }
                if goals::get(&id)
                    .map_err(|e| (-32000, e))?
                    .is_some_and(|goal| goal["status"] == "active")
                {
                    let state =
                        goals::handle("thread/goal/set", &json!({"threadId":id,"status":"paused"}))
                            .unwrap()
                            .map_err(|e| (-32000, e))?;
                    self.notify(
                        "thread/goal/updated",
                        json!({"threadId":id,"goal":state["goal"]}),
                    );
                }
                self.stop_thread(&id)?;
                let limit = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                while thread.is_active() {
                    if tokio::time::Instant::now() >= limit {
                        return Err((
                            -32000,
                            "The response is still stopping; retry the handoff when it has stopped"
                                .into(),
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            }
        }
        // is_active can become false before the event reader saves its final
        // accounting. Hold the execution lease only after that reader drains.
        let limit = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let _execution = loop {
            match thread_lifecycle::execution(&id) {
                Ok(lease) => break lease,
                Err(error) if !draining || tokio::time::Instant::now() >= limit => {
                    return Err((-32000, error))
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        };
        let cwd = meta["cwd"]
            .as_str()
            .ok_or_else(|| bad("The chat has no working directory"))?;
        let inspection = workspaces::handle("workspace/handoff/inspect", &json!({"cwd":cwd}))
            .await
            .unwrap()
            .map_err(|e| (-32000, e))?;
        let source_root = inspection["repository"]["repoRoot"]
            .as_str()
            .map(PathBuf::from)
            .ok_or_else(|| bad("The chat has no Git repository"))?;
        let target_root = if p["direction"] == "toLocal" {
            inspection["localCheckout"]["path"].as_str()
        } else {
            p["targetPath"].as_str()
        }
        .and_then(|p| PathBuf::from(p).canonicalize().ok());
        let busy = self.threads.lock().unwrap().iter().any(|(_, thread)| {
            thread.is_active()
                && PathBuf::from(&thread.cwd).canonicalize().is_ok_and(|path| {
                    path.starts_with(&source_root)
                        || target_root
                            .as_ref()
                            .is_some_and(|root| path.starts_with(root))
                })
        });
        if busy {
            return Err((-32000,"Stop other running responses in the source and target checkouts before handing off this chat".into()));
        }
        let mut params = p.clone();
        params["cwd"] = json!(cwd);
        params["threadId"] = json!(id);
        params["handoffRequestId"] = json!(operation);
        thread_lifecycle::mark_handoff(&mut reservation, operation).map_err(|e| (-32000, e))?;
        let server = self.clone();
        let progress_id = id.clone();
        let progress_operation = operation.to_string();
        let direction = p["direction"].clone();
        let prepared = workspaces::prepare_handoff(params, move |stage| {
            server.notify(
                "thread/handoff/progress",
                json!({"threadId":progress_id,"operationId":progress_operation,"direction":direction,"stage":stage}),
            )
        })
        .await
        .map_err(|e| (-32000, e))?;
        let target_cwd = prepared.info["cwd"].clone();
        let mut fork_params = json!({"threadId":id,"cwd":target_cwd});
        for key in [
            "model",
            "reasoningEffort",
            "sandbox",
            "sandboxPolicy",
            "approvalPolicy",
            "approvalsReviewer",
        ] {
            if let Some(value) = p.get(key) {
                fork_params[key] = value.clone();
            }
        }
        let mut warnings = Vec::new();
        let (fork, copied) = match self.fork_thread_locked(fork_params).await {
            Ok(result) => (result, true),
            Err(error) => {
                // Codex keeps a completed Git move when copying the chat fails.
                // The original chat remains open and owns the migrated worktree.
                warnings.push(format!(
                    "Git changes were handed off, but the chat could not be copied: {}",
                    error.1
                ));
                (json!({"thread":{"id":id}}), false)
            }
        };
        let target = fork["thread"]["id"]
            .as_str()
            .ok_or_else(|| bad("The copied chat has no ID"))?
            .to_string();
        if copied {
            let saved: std::result::Result<(), String> = (|| {
                let (session, _, _) = Session::open(Some(&target)).map_err(|e| e.to_string())?;
                if let Some(name) = meta["name"].as_str() {
                    session.try_set_name(name).map_err(|e| e.to_string())?;
                }
                desktop::handle("desktop/threadMetadata/set", &json!({"threadId":target,"pinned":metadata["pinned"]==true,"unread":metadata["unread"]==true})).unwrap()?;
                Ok(())
            })();
            if let Err(error) = saved {
                warnings.push(format!(
                    "Chat copied; some display settings could not be saved: {error}"
                ));
            }
        }
        let handoff = prepared.commit(&target).await.map_err(|e| (-32000, e))?;
        if copied {
            match source.archive() {
                Ok(()) => {
                    self.threads.lock().unwrap().remove(&id);
                    if let Err(error) = desktop::handle("desktop/threadMetadata/set", &json!({"threadId":id,"archived":true,"pinned":false})).unwrap() {
                        warnings.push(format!("Original chat archived; sidebar metadata could not be saved: {error}"));
                    }
                    match scheduler::delete_archived_heartbeats(&id) {
                        Ok(removed) if !removed.is_empty() => self.notify("scheduler/changed", json!({})),
                        Ok(_) => {},
                        Err(error) => warnings.push(format!("Original chat archived; heartbeat cleanup needs attention: {error}")),
                    }
                    self.notify("thread/archived", json!({"threadId":id}));
                }
                Err(error) => warnings.push(format!("Git and chat were handed off, but the original chat could not be archived: {error}")),
            }
        }
        warnings.extend(
            handoff["warnings"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(String::from),
        );
        // Persist warnings too, so timeout recovery has the same result as the
        // initial response. This does not change Git or the committed owner.
        let handoff = workspaces::handoff_warnings(handoff, &warnings)
            .await
            .map_err(|e| (-32000, e))?;
        self.notify("thread/handoff/progress", json!({"threadId":id,"operationId":operation,"direction":p["direction"],"stage":"ready","targetThreadId":target}));
        let thread = self
            .handle(
                "thread/read",
                json!({"threadId":target,"includeTurns":true}),
            )
            .await?["thread"]
            .clone();
        Ok(json!({"thread":thread,"sourceThreadId":id,"cwd":target_cwd,"handoff":handoff}))
    }

    pub(super) async fn inspect_handoff_thread(self: &Arc<Self>, p: Value) -> RpcResult {
        let thread = self
            .handle("thread/read", json!({"threadId":p["threadId"]}))
            .await?["thread"]
            .clone();
        let id = thread["id"]
            .as_str()
            .ok_or((-32602, "threadId is required".into()))?;
        let loaded = self.threads.lock().unwrap().get(id).cloned();
        let queued = loaded
            .as_ref()
            .map(|t| t.queued_inputs.lock().unwrap().len())
            .unwrap_or(0);
        let voice = loaded
            .as_ref()
            .is_some_and(|t| t.voice_call.lock().unwrap().is_some());
        let blocked = if thread["readOnly"] == true {
            Some("readOnly")
        } else if thread["mode"] != "codex" || thread["archived"] == true {
            Some("unavailable")
        } else if queued > 0 {
            Some("queuedFollowUps")
        } else if voice {
            Some("voiceCall")
        } else {
            None
        };
        let workspace =
            workspaces::handle("workspace/handoff/inspect", &json!({"cwd":thread["cwd"]}))
                .await
                .unwrap()
                .map_err(|e| (-32000, e))?;
        Ok(
            json!({"thread":thread,"workspace":workspace,"blockedReason":blocked,"queuedCount":queued,"voiceActive":voice}),
        )
    }

    pub(super) async fn handoff_thread_status(self: &Arc<Self>, p: Value) -> RpcResult {
        let operation = p["operationId"]
            .as_str()
            .ok_or((-32602, "operationId is required".into()))?;
        if let Some(id) = self.active_handoffs.lock().unwrap().get(operation).cloned() {
            if p["threadId"] != id {
                return Err((-32602, "This handoff belongs to another chat".into()));
            }
            return Ok(json!({"active":true,"threadId":id,"operation":null}));
        }
        let thread = self
            .handle("thread/read", json!({"threadId":p["threadId"]}))
            .await?["thread"]
            .clone();
        let status = workspaces::handle(
            "workspace/handoff/status",
            &json!({"cwd":thread["cwd"],"operationId":operation}),
        )
        .await
        .unwrap()
        .map_err(|e| (-32000, e))?;
        if !status["operation"].is_null()
            && !status["operation"]["sourceThreadId"].is_null()
            && status["operation"]["sourceThreadId"] != thread["id"]
        {
            return Err((-32602, "This handoff belongs to another chat".into()));
        }
        Ok(json!({"active":false,"operation":status["operation"]}))
    }
}
