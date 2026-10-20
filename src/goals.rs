//! Durable, thread-scoped goals. The dispatcher owns model turns and scheduling.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOCK: Mutex<()> = Mutex::new(());
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const RETRY_SECONDS: u64 = 10;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
enum Status {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Goal {
    thread_id: String,
    objective: String,
    status: Status,
    token_budget: Option<u64>,
    tokens_used: u64,
    time_used_seconds: u64,
    updated_at: u64,
    created_at: u64,
    in_flight: bool,
    turn_id: Option<String>,
    suppressed: bool,
    reason: Option<String>,
    evidence: Vec<String>,
    remaining: Vec<String>,
    next_prompt: Option<String>,
    active_since: Option<u64>,
    next_eligible_at: u64,
    blocker_key: Option<String>,
    blocker_count: u64,
    failure_key: Option<String>,
    failure_count: u64,
    #[serde(default)]
    turns_started: u64,
    #[serde(default)]
    turns_completed: u64,
    #[serde(default)]
    finished_turn_id: Option<String>,
}

impl Goal {
    fn new(thread_id: &str, objective: &str, budget: Option<u64>, now: u64) -> Self {
        Self {
            thread_id: thread_id.into(),
            objective: objective.into(),
            status: Status::Active,
            token_budget: budget,
            tokens_used: 0,
            time_used_seconds: 0,
            updated_at: now,
            created_at: now,
            in_flight: false,
            turn_id: None,
            suppressed: false,
            reason: None,
            evidence: vec![],
            remaining: vec![],
            next_prompt: None,
            active_since: Some(now),
            next_eligible_at: now,
            blocker_key: None,
            blocker_count: 0,
            failure_key: None,
            failure_count: 0,
            turns_started: 0,
            turns_completed: 0,
            finished_turn_id: None,
        }
    }

    fn budget_reached(&self) -> bool {
        self.token_budget
            .is_some_and(|limit| self.tokens_used >= limit)
    }

    fn set_status(&mut self, status: Status, now: u64) {
        if self.status == Status::Active && status != Status::Active {
            if let Some(since) = self.active_since.take() {
                self.time_used_seconds = self
                    .time_used_seconds
                    .saturating_add(now.saturating_sub(since));
            }
        } else if self.status != Status::Active && status == Status::Active {
            self.active_since = Some(now);
        }
        self.status = status;
        self.updated_at = now;
    }

    fn limit_budget(&mut self, now: u64) {
        self.set_status(Status::BudgetLimited, now);
        self.reason = Some("Goal token budget reached".into());
    }

    fn public(&self, now: u64) -> Value {
        let elapsed = if self.status == Status::Active {
            self.active_since
                .map(|start| now.saturating_sub(start))
                .unwrap_or(0)
        } else {
            0
        };
        // updatedAt is the timestamp of this projected time snapshot, so clients
        // can extrapolate active elapsed time without counting it twice.
        json!({
            "threadId": self.thread_id, "objective": self.objective, "status": self.status,
            "tokenBudget": self.token_budget, "tokensUsed": self.tokens_used,
            "timeUsedSeconds": self.time_used_seconds.saturating_add(elapsed),
            "updatedAt": if self.status == Status::Active { now } else { self.updated_at },
            "createdAt": self.created_at, "inFlight": self.in_flight, "turnId": self.turn_id,
            "suppressed": self.suppressed, "reason": self.reason,
            "evidence": self.evidence, "remaining": self.remaining, "nextPrompt": self.next_prompt,
            "nextEligibleAt": self.next_eligible_at,
            "turnsStarted": self.turns_started, "turnsCompleted": self.turns_completed,
        })
    }
}

#[derive(Deserialize, Serialize)]
struct State {
    version: u32,
    goals: BTreeMap<String, Goal>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            goals: BTreeMap::new(),
        }
    }
}

struct Store {
    path: PathBuf,
}

impl Store {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn transact<T>(
        &self,
        action: impl FnOnce(&mut State, u64) -> Result<(T, bool), String>,
    ) -> Result<T, String> {
        let _guard = LOCK
            .lock()
            .map_err(|_| "Goal state lock is unavailable".to_string())?;
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|e| format!("Could not create goal directory: {e}"))?;
        let mut options = private_options();
        options.read(true).write(true).create(true);
        let file_lock = options
            .open(self.path.with_extension("lock"))
            .map_err(|e| format!("Could not open goal lock: {e}"))?;
        file_lock
            .lock()
            .map_err(|e| format!("Could not lock goal state: {e}"))?;
        let mut state = match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice::<State>(&bytes)
                .map_err(|e| format!("Could not parse goal state: {e}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(format!("Could not read goal state: {e}")),
        };
        if state.version != 1 {
            return Err("Unsupported goal state version".into());
        }
        for (key, goal) in &state.goals {
            validate_objective(&goal.objective)?;
            if key != &goal.thread_id
                || key.trim().is_empty()
                || goal.token_budget == Some(0)
                || (goal.in_flight && goal.turn_id.as_deref().is_none_or(str::is_empty))
                || (goal.status == Status::Active) != goal.active_since.is_some()
                || goal.turns_completed > goal.turns_started
            {
                return Err("Invalid persisted goal state".into());
            }
        }
        let (response, changed) = action(&mut state, now_seconds())?;
        if changed {
            self.save(&state)?;
        }
        Ok(response)
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let temp = parent.join(format!(
            ".goals-{}-{}-{}.tmp",
            std::process::id(),
            now_seconds(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut options = private_options();
            options.write(true).create_new(true);
            let mut file = options
                .open(&temp)
                .map_err(|e| format!("Could not create goal state file: {e}"))?;
            let bytes = serde_json::to_vec_pretty(state)
                .map_err(|e| format!("Could not encode goal state: {e}"))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|e| format!("Could not write goal state: {e}"))?;
            drop(file);
            fs::rename(&temp, &self.path)
                .map_err(|e| format!("Could not replace goal state: {e}"))?;
            #[cfg(unix)]
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("Could not sync goal directory: {e}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn get(&self, thread_id: &str) -> Result<Option<Value>, String> {
        validate_id(thread_id, "threadId")?;
        self.transact(|state, now| Ok((state.goals.get(thread_id).map(|g| g.public(now)), false)))
    }

    fn pending(&self) -> Result<Vec<Value>, String> {
        self.transact(|state, now| {
            let mut goals: Vec<&Goal> = state
                .goals
                .values()
                .filter(|goal| {
                    goal.status == Status::Active
                        && !goal.in_flight
                        && !goal.suppressed
                        && !goal.budget_reached()
                        && goal.next_eligible_at <= now
                })
                .collect();
            goals.sort_by(|a, b| {
                a.updated_at
                    .cmp(&b.updated_at)
                    .then_with(|| a.thread_id.cmp(&b.thread_id))
            });
            Ok((
                goals.into_iter().map(|goal| goal.public(now)).collect(),
                false,
            ))
        })
    }

    fn set(&self, params: &Value) -> Result<Value, String> {
        let thread_id = required_text(params, "threadId")?;
        let objective = match params.get("objective") {
            Some(value) => {
                let text = value.as_str().ok_or("objective must be a string")?;
                validate_objective(text)?;
                Some(text)
            }
            None => None,
        };
        let status = match params.get("status").and_then(Value::as_str) {
            None if params.get("status").is_none() => None,
            Some("active") => Some(Status::Active),
            Some("paused") => Some(Status::Paused),
            _ => return Err("status must be active or paused".into()),
        };
        let budget = match params.get("tokenBudget") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => Some(Some(
                value
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or("tokenBudget must be null or a positive integer")?,
            )),
        };
        let replace = bool_param(params, "replace")?;
        let edit = bool_param(params, "edit")?;
        self.transact(|state, now| {
            let old = state.goals.get(thread_id);
            let different = old.is_some_and(|g| objective.is_some_and(|text| text != g.objective));
            if different && old.is_some_and(|g| g.status != Status::Complete) && !replace && !edit {
                return Err("An unfinished goal exists; set replace:true or edit:true".into());
            }
            let fresh = old.is_none() || replace || (different && !edit);
            let mut goal = if fresh {
                Goal::new(
                    thread_id,
                    objective.ok_or("objective is required for a new goal")?,
                    budget.unwrap_or(None),
                    now,
                )
            } else {
                old.unwrap().clone()
            };
            if let Some(text) = objective {
                goal.objective = text.into();
            }
            if let Some(limit) = budget {
                goal.token_budget = limit;
            }
            if let Some(status) = status {
                goal.set_status(status, now);
            }
            if status == Some(Status::Active) || (edit && different) {
                goal.suppressed = false;
                goal.next_eligible_at = now;
                goal.reason = None;
                goal.failure_key = None;
                goal.failure_count = 0;
                goal.blocker_key = None;
                goal.blocker_count = 0;
            }
            if status == Some(Status::Paused) {
                // The user may pause while a model turn is running. Its late finish
                // event must not restart the goal or leave it in flight.
                goal.in_flight = false;
                goal.suppressed = false;
            }
            if goal.status == Status::Active && goal.budget_reached() {
                goal.limit_budget(now);
            }
            goal.updated_at = now;
            let response = json!({"goal": goal.public(now)});
            state.goals.insert(thread_id.into(), goal);
            Ok((response, true))
        })
    }

    fn begin_turn(&self, thread_id: &str, turn_id: &str) -> Result<Option<Value>, String> {
        validate_id(thread_id, "threadId")?;
        validate_id(turn_id, "turnId")?;
        self.transact(|state, now| {
            let Some(goal) = state.goals.get_mut(thread_id) else {
                return Ok((None, false));
            };
            if goal.status != Status::Active {
                return Err("Goal is not active".into());
            }
            if goal.budget_reached() {
                return Err("Goal token budget reached".into());
            }
            if goal.in_flight {
                if goal.turn_id.as_deref() == Some(turn_id) {
                    return Ok((Some(goal.public(now)), false));
                }
                return Err("A goal turn is already running".into());
            }
            if goal.turn_id.as_deref() == Some(turn_id) {
                return Err("This goal turn was already finished or recovered".into());
            }
            if goal.suppressed {
                return Err("Resume the goal before starting another automatic turn".into());
            }
            if goal.next_eligible_at > now {
                return Err("Goal retry is waiting for backoff".into());
            }
            goal.in_flight = true;
            goal.turn_id = Some(turn_id.into());
            goal.turns_started = goal.turns_started.saturating_add(1);
            goal.updated_at = now;
            Ok((Some(goal.public(now)), true))
        })
    }

    fn add_usage(&self, thread_id: &str, tokens: u64) -> Result<Option<Value>, String> {
        validate_id(thread_id, "threadId")?;
        self.transact(|state, now| {
            let Some(goal) = state.goals.get_mut(thread_id) else {
                return Ok((None, false));
            };
            goal.tokens_used = goal.tokens_used.saturating_add(tokens);
            if goal.status == Status::Active && goal.budget_reached() {
                goal.limit_budget(now);
            }
            goal.updated_at = now;
            Ok((Some(goal.public(now)), true))
        })
    }

    fn usage_unavailable(&self, thread_id: &str) -> Result<Option<Value>, String> {
        validate_id(thread_id, "threadId")?;
        self.transact(|state, now| {
            let Some(goal) = state.goals.get_mut(thread_id) else {
                return Ok((None, false));
            };
            let changed = goal.status == Status::Active && goal.token_budget.is_some();
            if changed {
                goal.set_status(Status::UsageLimited, now);
                goal.reason = Some("Provider did not report token usage".into());
            }
            Ok((Some(goal.public(now)), changed))
        })
    }

    fn checkpoint(&self, thread_id: &str, report: &Value) -> Result<Value, String> {
        validate_id(thread_id, "threadId")?;
        if !report.is_object() {
            return Err("checkpoint report must be an object".into());
        }
        let status = match required_text(report, "status")? {
            "active" => Status::Active,
            "complete" => Status::Complete,
            "blocked" => Status::Blocked,
            _ => return Err("checkpoint status must be active, complete, or blocked".into()),
        };
        let summary = required_text(report, "summary")?;
        let evidence = string_list(report, "evidence")?;
        let remaining = string_list(report, "remaining")?;
        let next_prompt = optional_text(report, "nextPrompt")?;
        let blocker_key = optional_text(report, "blockerKey")?;
        if status == Status::Complete && (evidence.is_empty() || !remaining.is_empty()) {
            return Err("Completion requires evidence and no remaining work".into());
        }
        if status == Status::Blocked && blocker_key.is_none() {
            return Err("A blocked checkpoint requires blockerKey".into());
        }
        self.transact(|state, now| {
            let goal = state
                .goals
                .get_mut(thread_id)
                .ok_or("No goal exists for this thread")?;
            if goal.status != Status::Active {
                return Err("Only an active goal can accept a checkpoint".into());
            }
            if goal.budget_reached() {
                return Err("Goal token budget reached".into());
            }
            goal.evidence = evidence;
            goal.remaining = remaining;
            goal.next_prompt = next_prompt;
            goal.reason = Some(summary.into());
            if status == Status::Blocked {
                if goal.blocker_key == blocker_key {
                    goal.blocker_count = goal.blocker_count.saturating_add(1);
                } else {
                    goal.blocker_key = blocker_key;
                    goal.blocker_count = 1;
                }
                if goal.blocker_count >= 3 {
                    goal.set_status(Status::Blocked, now);
                }
            } else {
                goal.blocker_key = None;
                goal.blocker_count = 0;
                if status == Status::Complete {
                    goal.set_status(Status::Complete, now);
                }
            }
            goal.updated_at = now;
            Ok((goal.public(now), true))
        })
    }

    fn finish_turn(
        &self,
        thread_id: &str,
        turn_id: &str,
        result: &str,
        had_work_tools: bool,
        error: Option<&str>,
    ) -> Result<Option<Value>, String> {
        validate_id(thread_id, "threadId")?;
        validate_id(turn_id, "turnId")?;
        if !matches!(result, "completed" | "failed" | "interrupted") {
            return Err("Invalid goal turn result".into());
        }
        self.transact(|state, now| {
            let Some(goal) = state.goals.get_mut(thread_id) else { return Ok((None, false)); };
            if goal.turn_id.as_deref() != Some(turn_id) || goal.finished_turn_id.as_deref() == Some(turn_id) { return Ok((Some(goal.public(now)), false)); }
            goal.in_flight = false;
            goal.finished_turn_id = Some(turn_id.into());
            goal.turns_completed = goal.turns_completed.saturating_add(1);
            goal.updated_at = now;
            // Pausing and budget/usage/completion states take precedence over a
            // late model completion, including a completion after a user pause.
            if goal.status == Status::Active {
                match result {
                    "completed" => {
                        goal.failure_key = None; goal.failure_count = 0;
                        if !had_work_tools {
                            goal.suppressed = true;
                            goal.reason = Some("Automatic continuation stopped because the turn performed no work tools. Resume to continue.".into());
                        }
                    }
                    "interrupted" => {
                        goal.suppressed = true;
                        goal.reason = Some("Goal turn was interrupted. Resume to continue.".into());
                    }
                    "failed" => {
                        let cause = error.filter(|s| !s.trim().is_empty()).unwrap_or("Goal turn failed");
                        goal.reason = Some(cause.into());
                        let normalized = cause.trim().to_lowercase();
                        if ["429", "quota", "rate limit", "rate_limit", "usage limit"].iter().any(|text| normalized.contains(text)) {
                            goal.set_status(Status::UsageLimited, now);
                        } else {
                            if goal.failure_key.as_deref() == Some(normalized.as_str()) { goal.failure_count = goal.failure_count.saturating_add(1); }
                            else { goal.failure_key = Some(normalized); goal.failure_count = 1; }
                            if goal.failure_count >= 3 { goal.set_status(Status::Blocked, now); }
                            else { goal.next_eligible_at = now.saturating_add(RETRY_SECONDS); }
                        }
                    }
                    _ => unreachable!(),
                }
            }
            Ok((Some(goal.public(now)), true))
        })
    }

    fn recover_interrupted(&self) -> Result<(), String> {
        self.transact(|state, now| {
            let mut changed = false;
            for goal in state.goals.values_mut().filter(|g| g.in_flight) {
                goal.in_flight = false;
                // Consume the lost turn without claiming it finished; a stale
                // completion from it must not suppress resumed work.
                goal.finished_turn_id = goal.turn_id.clone();
                if goal.status == Status::Active {
                    goal.suppressed = false;
                    goal.next_eligible_at = now;
                    goal.reason = Some(
                        "Previous goal turn was interrupted by a restart; ready to continue".into(),
                    );
                }
                // Keep turnId and turn counters: a recovered turn is not a new
                // first request, and stale finish events must remain harmless.
                goal.updated_at = now;
                changed = true;
            }
            Ok(((), changed))
        })
    }
}

fn store() -> Store {
    Store::new(crate::config::home().join("goals.json"))
}

pub fn get(thread_id: &str) -> Result<Option<Value>, String> {
    store().get(thread_id)
}
pub fn pending() -> Result<Vec<Value>, String> {
    store().pending()
}
pub fn begin_turn(thread_id: &str, turn_id: &str) -> Result<Option<Value>, String> {
    store().begin_turn(thread_id, turn_id)
}
pub fn add_usage(thread_id: &str, tokens: u64) -> Result<Option<Value>, String> {
    store().add_usage(thread_id, tokens)
}
pub fn usage_unavailable(thread_id: &str) -> Result<Option<Value>, String> {
    store().usage_unavailable(thread_id)
}
pub fn checkpoint(thread_id: &str, report: &Value) -> Result<Value, String> {
    store().checkpoint(thread_id, report)
}
pub fn finish_turn(
    thread_id: &str,
    turn_id: &str,
    result: &str,
    had_work_tools: bool,
    error: Option<&str>,
) -> Result<Option<Value>, String> {
    store().finish_turn(thread_id, turn_id, result, had_work_tools, error)
}
pub fn recover_interrupted() -> Result<(), String> {
    store().recover_interrupted()
}

pub fn handle(method: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(
        method,
        "thread/goal/get" | "thread/goal/list" | "thread/goal/set" | "thread/goal/clear"
    ) {
        return None;
    }
    Some((|| {
        if !params.is_object() && !params.is_null() {
            return Err("Goal parameters must be an object".into());
        }
        let store = store();
        match method {
            "thread/goal/get" => {
                Ok(json!({"goal": store.get(required_text(params, "threadId")?)?}))
            }
            "thread/goal/set" => store.set(params),
            "thread/goal/clear" => {
                let id = required_text(params, "threadId")?;
                store.transact(|state, _| {
                    let changed = state.goals.remove(id).is_some();
                    Ok((json!({"goal": null}), changed))
                })
            }
            "thread/goal/list" => {
                let filter = params
                    .get("status")
                    .map(|s| {
                        serde_json::from_value::<Status>(s.clone())
                            .map_err(|_| "Invalid goal status".to_string())
                    })
                    .transpose()?;
                store.transact(|state, now| {
                    let mut goals: Vec<&Goal> = state.goals.values().filter(|g| filter.is_none_or(|s| g.status == s)).collect();
                    goals.sort_by(|a,b| a.updated_at.cmp(&b.updated_at).then_with(|| a.thread_id.cmp(&b.thread_id)));
                    Ok((json!({"data": goals.into_iter().map(|g| g.public(now)).collect::<Vec<_>>()}), false))
                })
            }
            _ => unreachable!(),
        }
    })())
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

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn validate_id(id: &str, name: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        Err(format!("{name} must not be empty"))
    } else {
        Ok(())
    }
}
fn validate_objective(text: &str) -> Result<(), String> {
    if text.trim().is_empty() || text.chars().count() > 4000 {
        Err("objective must contain 1 to 4000 characters".into())
    } else {
        Ok(())
    }
}
fn required_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} must be a non-empty string"))
}
fn optional_text(value: &Value, key: &str) -> Result<Option<String>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(|s| Some(s.into()))
            .ok_or_else(|| format!("{key} must be a non-empty string or null")),
    }
}
fn bool_param(value: &Value, key: &str) -> Result<bool, String> {
    match value.get(key) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| format!("{key} must be a boolean")),
    }
}
fn string_list(value: &Value, key: &str) -> Result<Vec<String>, String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{key} must be an array of non-empty strings"))?
        .iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
                .ok_or_else(|| format!("{key} must contain non-empty strings"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "mcodex-goals-test-{}-{}-{}",
                std::process::id(),
                now_seconds(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )))
        }
        fn store(&self) -> Store {
            Store::new(self.0.join("goals.json"))
        }
        fn create(&self, id: &str, budget: Option<u64>) -> Value {
            self.store().set(&json!({"threadId":id,"objective":"Finish with test evidence", "tokenBudget":budget})).unwrap()["goal"].clone()
        }
        fn report(status: &str) -> Value {
            json!({"status":status,"summary":"Checked current evidence","evidence":[],"remaining":["Run tests"],"nextPrompt":"Check the tests"})
        }
        fn advance_backoff(&self) {
            self.store()
                .transact(|state, _| {
                    for goal in state.goals.values_mut() {
                        goal.next_eligible_at = 0;
                    }
                    Ok(((), true))
                })
                .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn budget_uses_real_usage_and_cannot_be_completed_or_resumed_past_limit() {
        let f = Fixture::new();
        assert!(f.create("t", Some(20))["tokenBudget"].is_number());
        f.store().begin_turn("t", "turn-1").unwrap();
        assert_eq!(
            f.store().add_usage("t", 19).unwrap().unwrap()["status"],
            "active"
        );
        let goal = f.store().add_usage("t", 1).unwrap().unwrap();
        assert_eq!(goal["status"], "budgetLimited");
        assert_eq!(goal["tokensUsed"], 20);
        assert!(f.store().checkpoint("t", &json!({"status":"complete","summary":"Done","evidence":["test passed"],"remaining":[]})).is_err());
        f.store()
            .finish_turn("t", "turn-1", "completed", true, None)
            .unwrap();
        assert_eq!(
            f.store()
                .set(&json!({"threadId":"t","status":"active"}))
                .unwrap()["goal"]["status"],
            "budgetLimited"
        );
        assert!(f.store().pending().unwrap().is_empty());
        assert!(f.store().begin_turn("t", "turn-2").is_err());
        f.store()
            .set(&json!({"threadId":"t","status":"active","tokenBudget":null}))
            .unwrap();
        assert_eq!(f.store().get("t").unwrap().unwrap()["tokensUsed"], 20);
        assert_eq!(f.store().pending().unwrap().len(), 1);
    }

    #[test]
    fn complete_requires_evidence_and_no_remaining_work() {
        let f = Fixture::new();
        f.create("t", None);
        assert!(f
            .store()
            .checkpoint("t", &Fixture::report("complete"))
            .is_err());
        assert!(f
            .store()
            .checkpoint(
                "t",
                &json!({"status":"complete","summary":"Probably done","evidence":[],"remaining":[]})
            )
            .is_err());
        f.store()
            .checkpoint("t", &Fixture::report("active"))
            .unwrap();
        let done=f.store().checkpoint("t",&json!({"status":"complete","summary":"Verified","evidence":["cargo test passed"],"remaining":[]})).unwrap();
        assert_eq!(done["status"], "complete");
        assert!(f.store().pending().unwrap().is_empty());
    }

    #[test]
    fn same_blocker_needs_three_consecutive_reports_and_progress_resets_streak() {
        let f = Fixture::new();
        f.create("t", None);
        let mut report = Fixture::report("blocked");
        report["blockerKey"] = json!("missing-input");
        for _ in 0..2 {
            assert_eq!(
                f.store().checkpoint("t", &report).unwrap()["status"],
                "active"
            );
        }
        f.store()
            .checkpoint("t", &Fixture::report("active"))
            .unwrap();
        assert_eq!(
            f.store().checkpoint("t", &report).unwrap()["status"],
            "active"
        );
        report["blockerKey"] = json!("different-input");
        assert_eq!(
            f.store().checkpoint("t", &report).unwrap()["status"],
            "active"
        );
        assert_eq!(
            f.store().checkpoint("t", &report).unwrap()["status"],
            "active"
        );
        assert_eq!(
            f.store().checkpoint("t", &report).unwrap()["status"],
            "blocked"
        );
        assert!(f.store().pending().unwrap().is_empty());
    }

    #[test]
    fn no_work_tools_suppresses_continuation_until_explicit_resume() {
        let f = Fixture::new();
        f.create("t", None);
        f.store().begin_turn("t", "one").unwrap();
        f.store().begin_turn("t", "one").unwrap();
        let g = f
            .store()
            .finish_turn("t", "one", "completed", false, None)
            .unwrap()
            .unwrap();
        assert_eq!(g["status"], "active");
        assert_eq!(g["suppressed"], true);
        assert_eq!(g["turnsStarted"], 1);
        assert_eq!(g["turnsCompleted"], 1);
        assert_eq!(
            f.store()
                .finish_turn("t", "one", "completed", false, None)
                .unwrap()
                .unwrap()["turnsCompleted"],
            1
        );
        assert!(f.store().pending().unwrap().is_empty());
        f.store()
            .set(&json!({"threadId":"t","status":"active"}))
            .unwrap();
        f.store().begin_turn("t", "two").unwrap();
        f.store()
            .finish_turn("t", "one", "completed", true, None)
            .unwrap();
        assert_eq!(f.store().get("t").unwrap().unwrap()["inFlight"], true);
        f.store()
            .finish_turn("t", "two", "completed", true, None)
            .unwrap();
        assert_eq!(f.store().pending().unwrap().len(), 1);
        assert_eq!(f.store().get("t").unwrap().unwrap()["turnsCompleted"], 2);
    }

    #[test]
    fn reopen_recovers_in_flight_without_resetting_goal_usage_or_first_turn_identity() {
        let f = Fixture::new();
        f.create("t", Some(100));
        f.store().begin_turn("t", "lost-turn").unwrap();
        f.store().add_usage("t", 7).unwrap();
        f.store().recover_interrupted().unwrap();
        let g = f.store().get("t").unwrap().unwrap();
        assert_eq!(g["turnId"], "lost-turn");
        assert_eq!(g["turnsStarted"], 1);
        assert_eq!(g["tokensUsed"], 7);
        assert_eq!(g["inFlight"], false);
        assert_eq!(f.store().pending().unwrap().len(), 1);
        f.store().begin_turn("t", "resumed-turn").unwrap();
        let g = f
            .store()
            .finish_turn("t", "lost-turn", "completed", true, None)
            .unwrap()
            .unwrap();
        assert_eq!(g["turnId"], "resumed-turn");
        assert_eq!(g["inFlight"], true);
    }

    #[test]
    fn user_pause_clears_in_flight_and_late_finish_does_not_resume() {
        let f = Fixture::new();
        f.create("t", None);
        f.store().begin_turn("t", "one").unwrap();
        f.store()
            .set(&json!({"threadId":"t","status":"paused"}))
            .unwrap();
        let g = f
            .store()
            .finish_turn("t", "one", "interrupted", true, None)
            .unwrap()
            .unwrap();
        assert_eq!(g["status"], "paused");
        assert_eq!(g["inFlight"], false);
        assert_eq!(g["turnsCompleted"], 1);
        assert_eq!(
            f.store()
                .finish_turn("t", "one", "interrupted", true, None)
                .unwrap()
                .unwrap()["turnsCompleted"],
            1
        );
        f.store().recover_interrupted().unwrap();
        assert!(f.store().pending().unwrap().is_empty());
        f.store()
            .set(&json!({"threadId":"t","status":"active"}))
            .unwrap();
        f.store().begin_turn("t", "two").unwrap();
        let g = f
            .store()
            .finish_turn("t", "two", "interrupted", true, None)
            .unwrap()
            .unwrap();
        assert_eq!(g["status"], "active");
        assert_eq!(g["suppressed"], true);
    }

    #[test]
    fn failures_back_off_and_quota_and_missing_usage_have_distinct_limits() {
        let f = Fixture::new();
        f.create("t", Some(100));
        for index in 0..3 {
            let id = format!("turn-{index}");
            f.store().begin_turn("t", &id).unwrap();
            let g = f
                .store()
                .finish_turn("t", &id, "failed", true, Some("Network unreachable"))
                .unwrap()
                .unwrap();
            assert_eq!(g["status"], if index < 2 { "active" } else { "blocked" });
            assert!(f.store().pending().unwrap().is_empty());
            f.advance_backoff();
        }
        f.store()
            .set(&json!({"threadId":"t","status":"active"}))
            .unwrap();
        f.store().begin_turn("t", "quota").unwrap();
        assert_eq!(
            f.store()
                .finish_turn(
                    "t",
                    "quota",
                    "failed",
                    true,
                    Some("HTTP 429 quota exhausted")
                )
                .unwrap()
                .unwrap()["status"],
            "usageLimited"
        );
        f.store()
            .set(&json!({"threadId":"t","status":"active"}))
            .unwrap();
        assert_eq!(
            f.store().usage_unavailable("t").unwrap().unwrap()["status"],
            "usageLimited"
        );
        f.create("unbudgeted", None);
        assert_eq!(
            f.store().usage_unavailable("unbudgeted").unwrap().unwrap()["status"],
            "active"
        );
    }

    #[test]
    fn objective_replacement_is_explicit_and_edit_preserves_accounting() {
        let f = Fixture::new();
        f.create("t", Some(100));
        f.store().add_usage("t", 9).unwrap();
        assert!(f
            .store()
            .set(&json!({"threadId":"t","objective":"A different goal"}))
            .is_err());
        let g = f
            .store()
            .set(&json!({"threadId":"t","objective":"Edited goal","edit":true}))
            .unwrap();
        assert_eq!(g["goal"]["tokensUsed"], 9);
        assert_eq!(g["goal"]["tokenBudget"], 100);
        let g = f
            .store()
            .set(&json!({"threadId":"t","objective":"New goal","replace":true}))
            .unwrap();
        assert_eq!(g["goal"]["tokensUsed"], 0);
        assert!(g["goal"]["tokenBudget"].is_null());
        assert!(f
            .store()
            .set(&json!({"threadId":"t","objective":"汉".repeat(4001),"edit":true}))
            .is_err());
        assert!(f
            .store()
            .set(&json!({"threadId":"t","tokenBudget":0}))
            .is_err());
    }

    #[test]
    fn active_time_is_projected_without_rewriting_and_settled_on_pause() {
        let f = Fixture::new();
        f.create("t", None);
        f.store()
            .transact(|state, now| {
                let g = state.goals.get_mut("t").unwrap();
                g.active_since = Some(now - 12);
                g.time_used_seconds = 3;
                Ok(((), true))
            })
            .unwrap();
        let before = fs::read(f.store().path).unwrap();
        assert!(
            f.store().get("t").unwrap().unwrap()["timeUsedSeconds"]
                .as_u64()
                .unwrap()
                >= 15
        );
        assert_eq!(fs::read(f.store().path).unwrap(), before);
        let paused = f
            .store()
            .set(&json!({"threadId":"t","status":"paused"}))
            .unwrap();
        assert!(paused["goal"]["timeUsedSeconds"].as_u64().unwrap() >= 15);
        assert_eq!(
            f.store().get("t").unwrap().unwrap()["timeUsedSeconds"],
            paused["goal"]["timeUsedSeconds"]
        );
    }

    #[test]
    fn corrupt_state_is_preserved_and_unknown_methods_do_not_claim_other_rpcs() {
        let f = Fixture::new();
        f.create("t", None);
        fs::write(f.store().path, b"{broken").unwrap();
        assert!(f
            .store()
            .set(&json!({"threadId":"t","status":"active"}))
            .is_err());
        assert_eq!(fs::read(f.store().path).unwrap(), b"{broken");
        assert!(handle("thread/start", &json!({})).is_none());
    }

    #[test]
    fn file_lock_preserves_usage_from_independent_processes() {
        use std::process::{Command, Stdio};
        let f = Fixture::new();
        f.create("shared", None);
        let executable = std::env::current_exe().unwrap();
        let mut children = vec![];
        for _ in 0..3 {
            children.push(
                Command::new(&executable)
                    .args(["--exact", "goals::tests::process_usage_worker", "--ignored"])
                    .env("MCODEX_GOAL_TEST_STORE", &f.store().path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap(),
            );
        }
        for _ in 0..20 {
            f.store().add_usage("shared", 1).unwrap();
        }
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(f.store().get("shared").unwrap().unwrap()["tokensUsed"], 80);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&f.store().path).unwrap().permissions().mode() & 0o077,
                0
            );
            assert_eq!(
                fs::metadata(f.store().path.with_extension("lock"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
    }

    #[test]
    #[ignore = "launched with an isolated path by the cross-process locking test"]
    fn process_usage_worker() {
        let Some(path) = std::env::var_os("MCODEX_GOAL_TEST_STORE") else {
            return;
        };
        let store = Store::new(path.into());
        for _ in 0..20 {
            store.add_usage("shared", 1).unwrap();
        }
    }
}
