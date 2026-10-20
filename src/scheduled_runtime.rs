//! Executes durable scheduler claims through the same Agent/approval path as desktop turns.
use super::*;
use crate::scheduler;
use futures_util::FutureExt;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

static COMPLETED_TURNS: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

#[derive(Default)]
struct RunLinks {
    thread_ids: Vec<String>,
    worktrees: Vec<Value>,
}
impl RunLinks {
    fn patch(&self) -> Value {
        json!({"threadIds":self.thread_ids,"threadId":self.thread_ids.first(),"worktrees":self.worktrees})
    }
}

struct ChildTurn {
    thread_id: String,
    turn_id: String,
    label: String,
}
struct RunOutcome {
    status: &'static str,
    summary: String,
    error: Option<String>,
    has_update: bool,
}
impl RunOutcome {
    fn failed(error: String) -> Self {
        Self {
            status: "failed",
            summary: String::new(),
            error: Some(error),
            has_update: false,
        }
    }
}

impl Server {
    /// The caller must pass an owned claim from scheduler::claim_due/claim_run.
    pub(super) fn launch_scheduled(
        self: &Arc<Self>,
        claim: Value,
    ) -> std::result::Result<(), String> {
        let run_id = required(&claim["run"], "id")?.to_owned();
        if !claim["task"].is_object() {
            return Err("scheduled claim is missing its task".into());
        }
        let mut jobs = self
            .scheduled_jobs
            .lock()
            .map_err(|_| "scheduled jobs lock is poisoned")?;
        if jobs.get(&run_id).is_some_and(|job| !job.is_finished()) {
            return Err("this scheduled execution is already being monitored".into());
        }
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|e| format!("start scheduler worker: {e}"))?;
        let (ready, start) = oneshot::channel();
        let server = self.clone();
        let id = run_id.clone();
        let job = runtime.spawn(async move {
            if start.await.is_err() { return; }
            let mut links = RunLinks::default();
            let task = &claim["task"];
            let execution = std::panic::AssertUnwindSafe(server.execute_scheduled(task, &id, &mut links)).catch_unwind().await;
            let outcome = match execution {
                Ok(outcome) => outcome,
                Err(_) => {
                    server.interrupt_scheduled_threads(&id, &links.thread_ids).await;
                    RunOutcome::failed("The scheduled execution worker panicked; its turns were interrupted".into())
                }
            };
            if let Err(error) = server.finish_scheduled(task, &id, &links, outcome) {
                // A store/lease failure is visible; never turn a failed persistence write into success.
                server.notify("error", json!({"error":{"message":format!("Cannot finish scheduled run {id}: {error}")},"scheduledRunId":id,"willRetry":false}));
            }
            if let Ok(mut jobs) = server.scheduled_jobs.lock() { jobs.remove(&id); }
            forget_completed_run(&id);
        });
        jobs.insert(run_id.clone(), job);
        drop(jobs);
        if ready.send(()).is_err() {
            if let Ok(mut jobs) = self.scheduled_jobs.lock() {
                jobs.remove(&run_id);
            }
            let error = "The scheduled execution worker stopped before starting".to_string();
            let _ = scheduler::finish_run(&run_id, &json!({"status":"failed","error":error}));
            return Err(error);
        }
        Ok(())
    }

    /// Call from notify("turn/completed") after LiveTurn.apply, before publishing the notification.
    /// An exact-turn snapshot prevents the next user/Goal turn from overwriting a run's result.
    pub(super) fn capture_scheduled_completion(&self, params: &Value) {
        let Some(thread_id) = params["threadId"].as_str() else {
            return;
        };
        let thread = self
            .threads
            .lock()
            .ok()
            .and_then(|threads| threads.get(thread_id).cloned());
        let Some(thread) = thread else { return };
        let turn = thread
            .live
            .lock()
            .ok()
            .and_then(|live| live.as_ref().map(|live| live.turn.clone()));
        let Some(turn) = turn.filter(|turn| turn["id"] == params["turn"]["id"]) else {
            return;
        };
        let Some(run_id) = turn["scheduledRunId"].as_str() else {
            return;
        };
        if !self
            .scheduled_jobs
            .lock()
            .is_ok_and(|jobs| jobs.contains_key(run_id))
        {
            return;
        }
        if let Ok(mut completed) = COMPLETED_TURNS.lock() {
            let completed = completed.get_or_insert_with(HashMap::new);
            if let Some(turn_id) = turn["id"].as_str() {
                completed.insert(format!("{run_id}/{turn_id}"), turn);
            }
        }
    }

    pub(super) async fn cancel_scheduled(self: &Arc<Self>, run_id: &str) -> RpcResult {
        let bad = |message: String| (-32000, message);
        let job = self
            .scheduled_jobs
            .lock()
            .map_err(|_| bad("scheduled jobs lock is poisoned".into()))?
            .remove(run_id);
        if let Some(job) = job {
            job.abort();
            let _ = job.await;
        }
        let current = scheduler::update_run(run_id, &json!({})).map_err(bad)?;
        let run = &current["run"];
        if run["status"] != "running" {
            return Ok(current);
        }
        let ids = run["threadIds"]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let failures = self.interrupt_scheduled_threads(run_id, &ids).await;
        let task_id = required(run, "taskId").map_err(bad)?;
        let task = scheduler::handle("scheduler/read", &json!({"id":task_id}))
            .ok_or_else(|| bad("scheduler/read is unavailable".into()))?
            .map_err(bad)?;
        let message = if failures.is_empty() {
            "Scheduled execution stopped by the user".into()
        } else {
            format!("Scheduled execution stopped; {}", failures.join("; "))
        };
        let result = scheduler::finish_run(run_id, &json!({"status":"interrupted","summary":"Execution interrupted","error":message,"threadIds":ids,"worktrees":run["worktrees"]})).map_err(bad)?;
        self.notify("scheduler/run/updated", json!({"run":result["run"],"taskId":task_id,"shouldNotify":scheduled_should_notify(&task["task"],"interrupted",false)}));
        forget_completed_run(run_id);
        Ok(result)
    }

    /// Shutdown owns thread cleanup separately. Preserve run records for restart recovery.
    pub(super) fn stop_scheduled_jobs(&self) {
        if let Ok(mut jobs) = self.scheduled_jobs.lock() {
            for (_, job) in jobs.drain() {
                job.abort();
            }
        }
        if let Ok(mut completed) = COMPLETED_TURNS.lock() {
            *completed = None;
        }
    }

    async fn interrupt_scheduled_threads(
        self: &Arc<Self>,
        run_id: &str,
        ids: &[String],
    ) -> Vec<String> {
        let mut errors = Vec::new();
        for id in ids {
            let thread = self
                .threads
                .lock()
                .ok()
                .and_then(|threads| threads.get(id).cloned());
            let Some(thread) = thread else { continue };
            let _start = thread.start_lock.lock().await;
            let owned = thread.live.lock().is_ok_and(|live| {
                live.as_ref()
                    .is_some_and(|live| live.turn["scheduledRunId"] == run_id && live.is_active())
            });
            // A heartbeat may be waiting for an existing user turn. Do not interrupt that turn.
            if owned {
                if let Err((_, error)) = self.handle("turn/interrupt", json!({"threadId":id})).await
                {
                    errors.push(error);
                }
            }
        }
        errors
    }

    fn save_scheduled_links(
        &self,
        run_id: &str,
        links: &RunLinks,
    ) -> std::result::Result<(), String> {
        let result = scheduler::update_run(run_id, &links.patch())?;
        self.notify(
            "scheduler/run/updated",
            json!({"run":result["run"],"taskId":result["run"]["taskId"],"shouldNotify":false}),
        );
        Ok(())
    }

    async fn execute_scheduled(
        self: &Arc<Self>,
        task: &Value,
        run_id: &str,
        links: &mut RunLinks,
    ) -> RunOutcome {
        let projects = task["projects"]
            .as_array()
            .filter(|projects| !projects.is_empty())
            .cloned()
            .unwrap_or_else(|| vec![json!({"cwd":task["cwd"],"projectId":task["projectId"]})]);
        let heartbeat = task["kind"] == "heartbeat";
        if heartbeat && (projects.len() != 1 || task["executionEnvironment"] == "worktree") {
            return RunOutcome::failed("Heartbeat tasks use one existing chat and cannot create worktrees or run multiple projects".into());
        }
        let mut children = Vec::new();
        let mut errors = Vec::new();
        for (index, project) in projects.iter().enumerate() {
            match self
                .start_scheduled_child(task, project, run_id, index, links)
                .await
            {
                Ok(child) => children.push(child),
                Err(error) => errors.push(format!("Project {}: {error}", index + 1)),
            }
        }
        let mut summaries = Vec::new();
        let mut interrupted = false;
        let mut has_update = false;
        for child in &children {
            match self.wait_scheduled_turn(run_id, child).await {
                Ok(turn) => {
                    let (summary, update) = scheduled_turn_summary(&turn);
                    if !summary.is_empty() {
                        summaries.push(if projects.len() > 1 {
                            format!("{}\n{summary}", child.label)
                        } else {
                            summary
                        });
                    }
                    has_update |= update;
                    match turn["status"].as_str() {
                        Some("completed") => {}
                        Some("interrupted") => interrupted = true,
                        _ => errors.push(
                            turn["error"]["message"]
                                .as_str()
                                .or_else(|| turn["error"].as_str())
                                .unwrap_or("A scheduled Agent turn failed")
                                .to_string(),
                        ),
                    }
                }
                Err(error) => errors.push(error),
            }
        }
        let status = if !errors.is_empty() {
            "failed"
        } else if interrupted {
            "interrupted"
        } else {
            "completed"
        };
        if status == "completed" && task["kind"] == "cron" && task["autoArchive"] == true {
            for child in &children {
                let thread = self
                    .threads
                    .lock()
                    .ok()
                    .and_then(|threads| threads.get(&child.thread_id).cloned());
                if let Some(thread) = thread {
                    let _start = thread.start_lock.lock().await;
                    let still_ours = thread.live.lock().is_ok_and(|live| {
                        live.as_ref()
                            .is_some_and(|live| live.turn["id"] == child.turn_id)
                    });
                    if !thread.is_active() && still_ours {
                        match Session::open(Some(&child.thread_id))
                            .and_then(|(session, _, _)| session.archive())
                        {
                            Ok(()) => {
                                self.notify("thread/archived", json!({"threadId":child.thread_id}));
                            }
                            Err(error) => errors
                                .push(format!("Could not archive {}: {error}", child.thread_id)),
                        }
                    }
                }
            }
        }
        RunOutcome {
            status: if errors.is_empty() { status } else { "failed" },
            summary: summaries.join("\n\n"),
            error: if errors.is_empty() {
                if interrupted {
                    Some("A scheduled Agent turn was interrupted".into())
                } else {
                    None
                }
            } else {
                Some(errors.join("\n"))
            },
            has_update,
        }
    }

    async fn start_scheduled_child(
        self: &Arc<Self>,
        task: &Value,
        project: &Value,
        run_id: &str,
        index: usize,
        links: &mut RunLinks,
    ) -> std::result::Result<ChildTurn, String> {
        let heartbeat = task["kind"] == "heartbeat";
        let thread_id;
        let label = project["name"]
            .as_str()
            .or_else(|| project["cwd"].as_str())
            .unwrap_or("Project")
            .to_string();
        if heartbeat {
            thread_id = required(task, "targetThreadId")?.into();
            self.handle("thread/resume", json!({"threadId":thread_id}))
                .await
                .map_err(rpc_error)?;
            let thread = self
                .threads
                .lock()
                .map_err(|_| "threads lock is poisoned")?
                .get(&thread_id)
                .cloned()
                .ok_or("Heartbeat thread was not loaded")?;
            valid_directory(Path::new(&thread.cwd)).await?;
        } else {
            let mut cwd = valid_directory(Path::new(required(project, "cwd")?)).await?;
            if task["executionEnvironment"] == "worktree" {
                let plan =
                    plan_worktree(&cwd, &config::home().join("worktrees"), run_id, index).await?;
                links.worktrees.push(plan.metadata("planned", None));
                self.save_scheduled_links(run_id, links)?;
                let mut created = create_worktree(&plan).await;
                let mut metadata = plan.metadata(
                    if created.is_ok() { "ready" } else { "failed" },
                    created.as_ref().err().map(String::as_str),
                );
                if created.is_ok() {
                    match workspaces::register_created_worktree(
                        &plan.source_cwd,
                        &plan.path,
                        task["name"].as_str(),
                    )
                    .await
                    {
                        Ok(registered) => {
                            metadata["managedId"] = registered["worktree"]["id"].clone()
                        }
                        Err(error) => {
                            let message=format!("Worktree was created at {}, but registration failed: {error}. The checkout was retained.",plan.path.display());
                            metadata["status"] = json!("failed");
                            metadata["error"] = json!(message);
                            created = Err(message);
                        }
                    }
                }
                let last = links
                    .worktrees
                    .last_mut()
                    .ok_or("Missing worktree recovery metadata")?;
                *last = metadata;
                self.save_scheduled_links(run_id, links)?;
                created?;
                cwd = valid_directory(&plan.cwd).await?;
            }
            let result = self.handle("thread/start",json!({"mode":task["mode"],"cwd":cwd,"model":task["model"],"projectId":project.get("projectId").unwrap_or(&task["projectId"])})).await.map_err(rpc_error)?;
            thread_id = required(&result["thread"], "id")?.into();
        }
        if !links.thread_ids.contains(&thread_id) {
            links.thread_ids.push(thread_id.clone());
        }
        self.save_scheduled_links(run_id, links)?;
        let params = json!({"threadId":thread_id,"input":[{"type":"text","text":task["prompt"]}],"model":task["model"],"reasoningEffort":task["reasoningEffort"],"scheduledRunId":run_id});
        loop {
            let thread = self
                .threads
                .lock()
                .map_err(|_| "threads lock is poisoned")?
                .get(&thread_id)
                .cloned()
                .ok_or("Scheduled thread was unloaded before its turn started")?;
            if thread.is_active() {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            match self.handle("turn/start", params.clone()).await {
                Ok(result) => {
                    return Ok(ChildTurn {
                        thread_id,
                        turn_id: required(&result["turn"], "id")?.into(),
                        label,
                    })
                }
                Err((_, error))
                    if error.contains("already running") || error.contains("already active") =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await
                }
                Err(error) => return Err(rpc_error(error)),
            }
        }
    }

    async fn wait_scheduled_turn(
        &self,
        run_id: &str,
        child: &ChildTurn,
    ) -> std::result::Result<Value, String> {
        let completion_key = format!("{run_id}/{}", child.turn_id);
        loop {
            let completed = COMPLETED_TURNS.lock().ok().and_then(|turns| {
                turns
                    .as_ref()
                    .and_then(|turns| turns.get(&completion_key).cloned())
            });
            let thread = self
                .threads
                .lock()
                .map_err(|_| "threads lock is poisoned")?
                .get(&child.thread_id)
                .cloned()
                .ok_or("Scheduled thread disappeared while running")?;
            let live = thread
                .live
                .lock()
                .map_err(|_| "live turn lock is poisoned")?
                .as_ref()
                .map(|live| live.turn.clone());
            if let Some(turn) = completed {
                if live.as_ref().is_none_or(|live| live["id"] != child.turn_id)
                    || !thread.is_active()
                {
                    return Ok(turn);
                }
            }
            if let Some(turn) = live {
                if turn["id"] == child.turn_id && turn["scheduledRunId"] == run_id {
                    if matches!(
                        turn["status"].as_str(),
                        Some("completed" | "failed" | "interrupted")
                    ) && !thread.is_active()
                    {
                        return Ok(turn);
                    }
                } else {
                    // Recheck the completion cache after a scheduling boundary before reporting loss.
                    tokio::task::yield_now().await;
                    if let Some(turn) = COMPLETED_TURNS.lock().ok().and_then(|turns| {
                        turns
                            .as_ref()
                            .and_then(|turns| turns.get(&completion_key).cloned())
                    }) {
                        return Ok(turn);
                    }
                    return Err(format!(
                        "Scheduled turn {} was replaced before its final result was captured",
                        child.turn_id
                    ));
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn finish_scheduled(
        &self,
        task: &Value,
        run_id: &str,
        links: &RunLinks,
        outcome: RunOutcome,
    ) -> std::result::Result<(), String> {
        let mut patch = links.patch();
        patch["status"] = json!(outcome.status);
        patch["summary"] = json!(outcome.summary);
        patch["error"] = json!(outcome.error);
        patch["hasUpdate"] = json!(outcome.has_update);
        let result = scheduler::finish_run(run_id, &patch)?;
        self.notify("scheduler/run/updated",json!({"run":result["run"],"taskId":task["id"],"shouldNotify":scheduled_should_notify(task,outcome.status,outcome.has_update)}));
        Ok(())
    }
}

fn rpc_error(error: (i64, String)) -> String {
    error.1
}
fn required<'a>(value: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}
fn forget_completed_run(run_id: &str) {
    if let Ok(mut turns) = COMPLETED_TURNS.lock() {
        if let Some(turns) = turns.as_mut() {
            turns.retain(|_, turn| turn["scheduledRunId"] != run_id);
        }
    }
}
fn scheduled_should_notify(task: &Value, status: &str, has_update: bool) -> bool {
    let failed = matches!(status, "failed" | "interrupted");
    if task["notificationPolicy"] == "failed_runs_only" {
        failed
    } else {
        failed || task["kind"] != "heartbeat" || has_update
    }
}
fn scheduled_turn_summary(turn: &Value) -> (String, bool) {
    let text = turn["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["type"] == "agentMessage")
                .filter_map(|item| item["text"].as_str())
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let report = &turn["taskReport"];
    let summary = report["summary"]
        .as_str()
        .filter(|summary| !summary.trim().is_empty())
        .unwrap_or(&text)
        .to_string();
    let has_update = report["hasUpdate"]
        .as_bool()
        .unwrap_or(!summary.trim().is_empty());
    (summary, has_update)
}

struct WorktreePlan {
    source_cwd: PathBuf,
    repository: PathBuf,
    path: PathBuf,
    cwd: PathBuf,
    branch: String,
    head: String,
}
impl WorktreePlan {
    fn metadata(&self, status: &str, error: Option<&str>) -> Value {
        json!({"path":self.path,"cwd":self.cwd,"sourceCwd":self.source_cwd,"repository":self.repository,"branch":self.branch,"head":self.head,"status":status,"error":error})
    }
}
async fn valid_directory(path: &Path) -> std::result::Result<PathBuf, String> {
    let path = tokio::fs::canonicalize(path)
        .await
        .map_err(|e| format!("Invalid working directory {}: {e}", path.display()))?;
    if path.to_str().is_none() {
        return Err("The working directory cannot be represented as a UTF-8 RPC path".into());
    }
    if !tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err(format!(
            "Working directory is not a directory: {}",
            path.display()
        ));
    }
    Ok(path)
}
async fn git_output(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        Command::new("git")
            .env_remove("GIT_DIR")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("git {} timed out", args.join(" ")))?
    .map_err(|e| format!("Run git in {}: {e}", cwd.display()))?;
    if !output.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string())
}
async fn plan_worktree(
    cwd: &Path,
    home: &Path,
    run_id: &str,
    index: usize,
) -> std::result::Result<WorktreePlan, String> {
    if run_id.is_empty()
        || run_id.len() > 160
        || !run_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
    {
        return Err("Invalid scheduled run ID for a worktree".into());
    }
    let source_cwd = valid_directory(cwd).await?;
    let repository = valid_directory(Path::new(
        &git_output(&source_cwd, &["rev-parse", "--show-toplevel"]).await?,
    ))
    .await?;
    let relative = source_cwd
        .strip_prefix(&repository)
        .map_err(|_| "Working directory is outside its Git root")?;
    let head = git_output(&repository, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let path = home.join(run_id).join(index.to_string());
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    };
    Ok(WorktreePlan {
        cwd: path.join(relative),
        path,
        source_cwd,
        repository,
        branch: format!("codex/scheduled-{run_id}-{index}"),
        head,
    })
}
async fn create_worktree(plan: &WorktreePlan) -> std::result::Result<(), String> {
    let parent = plan.path.parent().ok_or("Worktree path has no parent")?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| format!("Create worktree parent: {e}"))?;
    if tokio::fs::symlink_metadata(&plan.path).await.is_ok() {
        return Err(format!(
            "Worktree destination already exists: {}",
            plan.path.display()
        ));
    }
    let path = plan
        .path
        .to_str()
        .ok_or("Worktree destination is not UTF-8")?;
    git_output(
        &plan.repository,
        &[
            "worktree",
            "add",
            "-b",
            &plan.branch,
            "--",
            path,
            &plan.head,
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "mcodex-scheduled-runtime-{:032x}",
                rand::random::<u128>()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    async fn repository(root: &Path) -> PathBuf {
        let repo = root.join("source");
        tokio::fs::create_dir_all(repo.join("nested"))
            .await
            .unwrap();
        git_output(&repo, &["init"]).await.unwrap();
        tokio::fs::write(repo.join("nested/tracked.txt"), "committed")
            .await
            .unwrap();
        git_output(&repo, &["add", "."]).await.unwrap();
        git_output(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        )
        .await
        .unwrap();
        repo
    }
    #[tokio::test]
    async fn worktrees_start_from_committed_head_and_map_repository_subdirectories() {
        let fixture = Fixture::new();
        let repo = repository(&fixture.0).await;
        let original_branch = git_output(&repo, &["branch", "--show-current"])
            .await
            .unwrap();
        tokio::fs::write(repo.join("nested/tracked.txt"), "uncommitted user change")
            .await
            .unwrap();
        tokio::fs::write(repo.join("private-untracked.txt"), "keep local")
            .await
            .unwrap();
        let plan = plan_worktree(
            &repo.join("nested"),
            &fixture.0.join("worktrees"),
            "run_isolated",
            0,
        )
        .await
        .unwrap();
        create_worktree(&plan).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(plan.cwd.join("tracked.txt"))
                .await
                .unwrap(),
            "committed"
        );
        assert!(!plan.path.join("private-untracked.txt").exists());
        assert_eq!(
            git_output(&plan.path, &["branch", "--show-current"])
                .await
                .unwrap(),
            "codex/scheduled-run_isolated-0"
        );
        assert_eq!(
            git_output(&repo, &["branch", "--show-current"])
                .await
                .unwrap(),
            original_branch
        );
        assert_eq!(
            tokio::fs::read_to_string(repo.join("nested/tracked.txt"))
                .await
                .unwrap(),
            "uncommitted user change"
        );
        assert_eq!(plan.metadata("ready", None)["cwd"], json!(plan.cwd));
    }
    #[tokio::test]
    async fn failed_worktree_creation_keeps_recovery_information_and_does_not_overwrite() {
        let fixture = Fixture::new();
        let repo = repository(&fixture.0).await;
        let plan = plan_worktree(&repo, &fixture.0.join("worktrees"), "run_conflict", 1)
            .await
            .unwrap();
        git_output(&repo, &["branch", &plan.branch]).await.unwrap();
        let error = create_worktree(&plan).await.unwrap_err();
        let record = plan.metadata("failed", Some(&error));
        assert_eq!(record["path"], json!(plan.path));
        assert_eq!(record["branch"], plan.branch);
        assert!(record["error"].as_str().unwrap().contains("already exists"));
        assert!(!plan.path.exists());
        assert!(
            plan_worktree(&repo, &fixture.0.join("worktrees"), "../escape", 0)
                .await
                .is_err()
        );
    }
    #[test]
    fn explicit_no_update_is_a_successful_quiet_heartbeat() {
        let turn = json!({"status":"completed","items":[{"type":"agentMessage","text":"No changes"}],"taskReport":{"summary":"No changes","hasUpdate":false}});
        let (summary, update) = scheduled_turn_summary(&turn);
        assert_eq!(summary, "No changes");
        assert!(!update);
        assert!(!scheduled_should_notify(
            &json!({"kind":"heartbeat"}),
            "completed",
            update
        ));
        assert!(scheduled_should_notify(
            &json!({"kind":"cron"}),
            "completed",
            update
        ));
        assert!(!scheduled_should_notify(
            &json!({"kind":"cron","notificationPolicy":"failed_runs_only"}),
            "completed",
            true
        ));
        assert!(scheduled_should_notify(
            &json!({"kind":"heartbeat","notificationPolicy":"failed_runs_only"}),
            "interrupted",
            false
        ));
    }
}
