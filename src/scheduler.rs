//! Durable local schedules and execution records. This module never calls a model.
//! Scheduled claims consume the calendar; manual claims leave the calendar unchanged.
//! A task's `completed` state means its calendar is exhausted, not that a run succeeded.

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use rrule::{Frequency, RRule, RRuleSet, Tz, Unvalidated};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, String>;
static STORE_LOCK: Mutex<()> = Mutex::new(());
static DISPATCH_OWNERS: Mutex<Option<HashMap<PathBuf, File>>> = Mutex::new(None);
const MIN_INTERVAL_MS: i64 = 60_000;
const MAX_SCAN: u16 = u16::MAX;
const METHODS: &[&str] = &[
    "scheduler/list",
    "scheduler/read",
    "scheduler/create",
    "scheduler/update",
    "scheduler/delete",
    "scheduler/runs",
    "scheduler/run/update",
];

pub fn handle(method: &str, params: &Value) -> Option<Result<Value>> {
    METHODS
        .contains(&method)
        .then(|| Store::configured().rpc(method, params, now_ms()))
}

pub fn claim_due(now_ms: u64) -> Result<Vec<Value>> {
    let store = Store::configured();
    ensure_dispatch_owner(&store, now_ms)?;
    store.claim_due(now_ms)
}

pub fn claim_run(id: &str, now_ms: u64) -> Result<Value> {
    let store = Store::configured();
    ensure_dispatch_owner(&store, now_ms)?;
    store.claim_run(id, now_ms)
}

pub fn finish_run(run_id: &str, patch: &Value) -> Result<Value> {
    let store = Store::configured();
    let now = now_ms();
    ensure_dispatch_owner(&store, now)?;
    store.finish_run(run_id, patch, now)
}

/// Save running thread/worktree links or review fields without fabricating success.
/// Only `finish_run` can change a run's lifecycle status.
pub fn update_run(run_id: &str, patch: &Value) -> Result<Value> {
    Store::configured().update_run(run_id, patch, now_ms())
}

pub fn recover_interrupted() -> Result<()> {
    ensure_dispatch_owner(&Store::configured(), now_ms())
}

// Keep this lease open for the process lifetime. A second desktop/preview
// process may edit schedules, but must not recover or execute the live owner's jobs.
fn ensure_dispatch_owner(store: &Store, now: u64) -> Result<()> {
    let mut owners = DISPATCH_OWNERS
        .lock()
        .map_err(|_| "scheduler owner lock is poisoned")?;
    let owners = owners.get_or_insert_with(HashMap::new);
    let parent = store.path.parent().ok_or("schedule store has no parent")?;
    fs::create_dir_all(parent).map_err(|e| format!("create scheduler directory: {e}"))?;
    let key = parent
        .canonicalize()
        .map_err(|e| format!("resolve scheduler directory: {e}"))?;
    if owners.contains_key(&key) {
        return Ok(());
    }
    let path = key.join("schedules.dispatch.lock");
    reject_symlink(&path)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lease = options
        .open(path)
        .map_err(|e| format!("open scheduler execution lease: {e}"))?;
    lease.try_lock().map_err(|e| {
        format!("another process owns this scheduler; keep one desktop scheduler running: {e}")
    })?;
    store.recover_interrupted(now)?;
    owners.insert(key, lease);
    Ok(())
}

fn now_ms() -> u64 {
    Utc::now().timestamp_millis().max(0) as u64
}
fn new_id(prefix: &str) -> String {
    format!("{prefix}_{:032x}", rand::random::<u128>())
}
fn text<'a>(object: &'a Value, field: &str) -> Result<&'a str> {
    object[field]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{field} must be a non-empty string"))
}
fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| "parameters must be an object".into())
}
fn one_of(value: &Value, field: &str, allowed: &[&str]) -> Result<()> {
    if !value[field].as_str().is_some_and(|s| allowed.contains(&s)) {
        return Err(format!("{field} must be {}", allowed.join(" or ")));
    }
    Ok(())
}
fn optional_text(value: &Value, field: &str) -> Result<()> {
    if value
        .get(field)
        .is_some_and(|v| !v.is_null() && !v.as_str().is_some_and(|s| !s.trim().is_empty()))
    {
        return Err(format!("{field} must be a non-empty string or null"));
    }
    Ok(())
}
fn timestamp(value: u64) -> Result<DateTime<Tz>> {
    let value = i64::try_from(value).map_err(|_| "timestamp is out of range")?;
    Utc.timestamp_millis_opt(value)
        .single()
        .map(|date| date.with_timezone(&Tz::UTC))
        .ok_or_else(|| "timestamp is out of range".into())
}
fn millis(date: &DateTime<Tz>) -> Result<u64> {
    u64::try_from(date.timestamp_millis())
        .map_err(|_| "schedule dates before 1970 are unsupported".into())
}
fn dtstart(date: &DateTime<Tz>) -> String {
    format!(
        "DTSTART;TZID={}:{}",
        date.timezone().name(),
        date.format("%Y%m%dT%H%M%S")
    )
}

struct Calendar {
    schedule: Value,
    // UTC is used only as a container for local wall-clock fields. Mapping to
    // the real timezone happens after candidate generation, before COUNT/UNTIL.
    set: RRuleSet,
    timezone: Tz,
    count: Option<u32>,
    until: Option<DateTime<Tz>>,
    recurring: bool,
}

fn calendar(schedule: &Value) -> Result<Calendar> {
    object(schedule)?;
    let zone = text(schedule, "timezone")?;
    if zone.contains(['\n', '\r', ':', ';']) {
        return Err("timezone must be an IANA timezone name".into());
    }
    let probe: RRuleSet = format!("DTSTART;TZID={zone}:20200115T120000\nRRULE:FREQ=DAILY;COUNT=1")
        .parse()
        .map_err(|e| format!("invalid IANA timezone: {e}"))?;
    let timezone = probe.get_dt_start().timezone();
    let line = text(schedule, "dtstart")?.trim();
    if line.contains(['\n', '\r']) || !line.starts_with("DTSTART") {
        return Err("dtstart must be one ICS DTSTART line".into());
    }
    let (header, value) = line.split_once(':').ok_or("invalid DTSTART line")?;
    let mut tzid = None;
    let mut value_type = None;
    for (index, parameter) in header.split(';').enumerate() {
        if index == 0 {
            if parameter != "DTSTART" {
                return Err("invalid DTSTART property".into());
            }
        } else if let Some(zone) = parameter.strip_prefix("TZID=") {
            if tzid.replace(zone).is_some() {
                return Err("DTSTART must not repeat TZID".into());
            }
        } else if let Some(kind) = parameter.strip_prefix("VALUE=") {
            if !["DATE", "DATE-TIME"].contains(&kind) || value_type.replace(kind).is_some() {
                return Err("invalid or repeated DTSTART VALUE".into());
            }
        } else {
            return Err("invalid DTSTART parameters".into());
        }
    }
    if tzid.is_some_and(|actual| actual != zone) {
        return Err("DTSTART TZID must match schedule.timezone".into());
    }
    if value.ends_with('Z') && tzid.is_some() {
        return Err("a UTC DTSTART must not also specify TZID".into());
    }
    let raw = value.strip_suffix('Z').unwrap_or(value);
    let naive = if raw.len() == 8 {
        if value_type == Some("DATE-TIME") {
            return Err("DTSTART VALUE does not match its date".into());
        }
        NaiveDate::parse_from_str(raw, "%Y%m%d")
            .map_err(|e| format!("invalid DTSTART: {e}"))?
            .and_hms_opt(0, 0, 0)
            .ok_or("invalid DTSTART date")?
    } else {
        if value_type == Some("DATE") {
            return Err("DTSTART VALUE does not match its datetime".into());
        }
        NaiveDateTime::parse_from_str(raw, "%Y%m%dT%H%M%S")
            .map_err(|e| format!("invalid DTSTART: {e}"))?
    };
    let wall_start = if value.ends_with('Z') {
        let instant = Utc.from_utc_datetime(&naive).with_timezone(&Tz::UTC);
        millis(&instant)?;
        instant.with_timezone(&timezone).naive_local()
    } else {
        naive
    };
    let wall_utc = Utc.from_utc_datetime(&wall_start).with_timezone(&Tz::UTC);
    let raw_rule = match schedule.get("rrule") {
        None | Some(Value::Null) => None,
        Some(Value::String(rule)) if !rule.trim().is_empty() => {
            if rule.contains(['\n', '\r']) {
                return Err("rrule must be one RFC5545 rule".into());
            }
            let uppercase = rule.trim().to_ascii_uppercase();
            let rule = uppercase
                .strip_prefix("RRULE:")
                .unwrap_or(&uppercase)
                .to_string();
            let mut seen = std::collections::HashSet::new();
            for part in rule.split(';') {
                let (key, value) = part.split_once('=').ok_or("invalid RRULE component")?;
                if key.is_empty() || value.is_empty() || !seen.insert(key) {
                    return Err("RRULE components must be non-empty and must not repeat".into());
                }
                if key == "UNTIL" && !value.ends_with('Z') {
                    return Err("UNTIL must be a UTC date-time ending in Z".into());
                }
            }
            if seen.contains("COUNT") && seen.contains("UNTIL") {
                return Err("RRULE cannot combine COUNT and UNTIL".into());
            }
            Some(rule)
        }
        _ => return Err("rrule must be an RFC5545 string or null".into()),
    };
    let recurring = raw_rule.is_some();
    let (set, count, until) = if let Some(raw_rule) = &raw_rule {
        let original: RRule<Unvalidated> = raw_rule
            .parse()
            .map_err(|e| format!("invalid recurrence rule: {e}"))?;
        if original.get_interval() == 0 || original.get_count() == Some(0) {
            return Err("RRULE INTERVAL and COUNT must be positive".into());
        }
        let count = original.get_count();
        let until = original.get_until().copied();
        // The library counts its normalized DST-gap candidates. Keep COUNT and
        // UNTIL outside that iterator so only real local instants consume them.
        // Each bounded evaluation still has MAX_SCAN and the library's own limit.
        let candidate_rule = raw_rule
            .split(';')
            .filter(|part| !part.starts_with("COUNT=") && !part.starts_with("UNTIL="))
            .collect::<Vec<_>>()
            .join(";");
        let validated = candidate_rule
            .parse::<RRule<Unvalidated>>()
            .map_err(|e| format!("invalid recurrence rule: {e}"))?
            .validate(wall_utc)
            .map_err(|e| format!("invalid recurrence rule: {e}"))?;
        if (validated.get_freq() == Frequency::Secondly && validated.get_interval() < 60)
            || validated.get_by_second().len() > 1
        {
            return Err("recurrences must be at least 60 seconds apart".into());
        }
        (RRuleSet::new(wall_utc).rrule(validated), count, until)
    } else {
        (RRuleSet::new(wall_utc).rdate(wall_utc), None, None)
    };
    let normalized_rule = if recurring {
        Some(rule_with_bounds(
            &set.get_rrule()[0].to_string(),
            count,
            until.as_ref(),
        ))
    } else {
        None
    };
    Ok(Calendar {
        schedule: json!({"dtstart":format!("DTSTART;TZID={zone}:{}",wall_start.format("%Y%m%dT%H%M%S")),"rrule":normalized_rule,"timezone":zone}),
        set,
        timezone,
        count,
        until,
        recurring,
    })
}

fn rule_with_bounds(base: &str, count: Option<u32>, until: Option<&DateTime<Tz>>) -> String {
    let mut parts = vec![base.to_string()];
    if let Some(count) = count {
        parts.push(format!("COUNT={count}"));
    }
    if let Some(until) = until {
        parts.push(format!(
            "UNTIL={}",
            until.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
        ));
    }
    parts.join(";")
}

struct OccurrenceBatch {
    dates: Vec<DateTime<Tz>>,
    exhausted: bool,
}

fn matches_wall_time(calendar: &Calendar, wall: &NaiveDateTime) -> bool {
    let Some(rule) = calendar.set.get_rrule().first() else {
        return true;
    };
    (rule.get_by_hour().is_empty() || rule.get_by_hour().contains(&(wall.hour() as u8)))
        && (rule.get_by_minute().is_empty()
            || rule.get_by_minute().contains(&(wall.minute() as u8)))
        && (rule.get_by_second().is_empty()
            || rule.get_by_second().contains(&(wall.second() as u8)))
}

/// RFC5545 3.3.10: nonexistent local recurrence instances do not consume COUNT.
/// RFC5545 3.3.5: a repeated local datetime refers to its first occurrence.
/// Generate wall-clock candidates in UTC so the library cannot normalize a gap
/// into another allowed BYHOUR (or consume COUNT before we reject that date).
fn occurrence_batch(calendar: &Calendar, limit: u16) -> Result<OccurrenceBatch> {
    let raw = calendar.set.clone().all(limit);
    if raw.limited && raw.dates.len() < usize::from(limit) {
        return Err(
            "recurrence evaluation hit the library's validation limit; simplify the rule".into(),
        );
    }
    let mut dates: Vec<DateTime<Tz>> = Vec::new();
    let mut exhausted = !raw.limited;
    for candidate in raw.dates {
        let wall = candidate.naive_utc();
        if !matches_wall_time(calendar, &wall) {
            continue;
        }
        let mapped = match calendar.timezone.from_local_datetime(&wall) {
            LocalResult::None => continue,
            LocalResult::Single(date) => date,
            LocalResult::Ambiguous(first, second) => first.min(second),
        };
        // This also defends the wall-clock fields against timezone normalization.
        if mapped.naive_local() != wall {
            continue;
        }
        if calendar.until.is_some_and(|until| mapped > until) {
            exhausted = true;
            break;
        }
        millis(&mapped)?;
        if dates.last().is_some_and(|previous| {
            mapped.timestamp_millis() - previous.timestamp_millis() < MIN_INTERVAL_MS
        }) {
            return Err("recurrences must be at least 60 seconds apart".into());
        }
        dates.push(mapped);
        if calendar
            .count
            .is_some_and(|count| dates.len() as u32 >= count)
        {
            exhausted = true;
            break;
        }
    }
    Ok(OccurrenceBatch { dates, exhausted })
}

#[derive(Debug)]
struct NextOccurrence {
    next: Option<u64>,
    cursor: Option<Value>,
}

fn next_after(calendar: &Calendar, after_ms: u64) -> Result<NextOccurrence> {
    timestamp(after_ms)?;
    let mut limit = 32u16;
    loop {
        let generated = occurrence_batch(calendar, limit)?;
        if let Some(index) = generated
            .dates
            .iter()
            .position(|date| date.timestamp_millis() > after_ms as i64)
        {
            let cursor = if index > 0
                && calendar.recurring
                && calendar.set.get_rrule()[0].get_freq() != Frequency::Yearly
            {
                let count = calendar
                    .count
                    .map(|count| {
                        count
                            .checked_sub((index - 1) as u32)
                            .ok_or("invalid recurrence COUNT cursor")
                    })
                    .transpose()?;
                let rule = rule_with_bounds(
                    &calendar.set.get_rrule()[0].to_string(),
                    count,
                    calendar.until.as_ref(),
                );
                Some(
                    json!({"dtstart":dtstart(&generated.dates[index-1]),"rrule":rule,"timezone":calendar.timezone.name()}),
                )
            } else {
                None
            };
            return Ok(NextOccurrence {
                next: Some(millis(&generated.dates[index])?),
                cursor,
            });
        }
        if generated.exhausted {
            return Ok(NextOccurrence {
                next: None,
                cursor: None,
            });
        }
        if limit == MAX_SCAN {
            return Err(
                "too many missed occurrences to evaluate safely; choose a later DTSTART".into(),
            );
        }
        limit = limit.saturating_mul(2);
    }
}

fn first_occurrence(calendar: &Calendar) -> Result<u64> {
    let mut limit = 32u16;
    loop {
        let generated = occurrence_batch(calendar, limit)?;
        if let Some(first) = generated.dates.first() {
            return millis(first);
        }
        if generated.exhausted {
            return Err("recurrence rule has no valid local occurrences".into());
        }
        if limit == MAX_SCAN {
            return Err(
                "recurrence evaluation exceeded its candidate budget; simplify the rule".into(),
            );
        }
        limit = limit.saturating_mul(2);
    }
}

#[derive(Clone)]
struct Store {
    path: PathBuf,
}

impl Store {
    fn configured() -> Self {
        Self {
            path: crate::config::home().join("schedules.json"),
        }
    }

    fn transaction<T>(&self, action: impl FnOnce(&mut Value) -> Result<(T, bool)>) -> Result<T> {
        let _guard = STORE_LOCK
            .lock()
            .map_err(|_| "scheduler store lock is poisoned")?;
        let parent = self.path.parent().ok_or("schedule store has no parent")?;
        fs::create_dir_all(parent).map_err(|e| format!("create scheduler directory: {e}"))?;
        let lock_path = parent.join("schedules.lock");
        reject_symlink(&lock_path)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(lock_path)
            .map_err(|e| format!("open scheduler lock: {e}"))?;
        let started = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(e) if started.elapsed() >= Duration::from_secs(2) => {
                    return Err(format!("scheduler store is busy or cannot be locked: {e}"))
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        let mut state = self.load()?;
        let (result, changed) = action(&mut state)?;
        if changed {
            self.save(&state)?;
        }
        Ok(result) // Dropping the file also releases its cross-process lock.
    }

    fn load(&self) -> Result<Value> {
        reject_symlink(&self.path)?;
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"version":1,"tasks":{},"runs":{},"inFlight":{},"cursors":{}}))
            }
            Err(e) => return Err(format!("read schedule store: {e}")),
        };
        let state: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("invalid schedule store JSON: {e}"))?;
        if state["version"] != 1 {
            return Err("unsupported schedule store version".into());
        }
        for collection in ["tasks", "runs", "inFlight", "cursors"] {
            if !state[collection].is_object() {
                return Err(format!(
                    "invalid scheduler store: {collection} must be an object"
                ));
            }
        }
        for collection in ["tasks", "runs"] {
            for (id, item) in state[collection].as_object().unwrap() {
                if !item.is_object() || item["id"] != id.as_str() {
                    return Err(format!("invalid scheduler store {collection} entry {id}"));
                }
            }
        }
        for task in state["tasks"].as_object().unwrap().values() {
            one_of(task, "state", &["active", "paused", "completed"])?;
            if !task["nextRunAt"].is_null() && task["nextRunAt"].as_u64().is_none() {
                return Err(
                    "invalid scheduler store: nextRunAt must be a timestamp or null".into(),
                );
            }
        }
        for run in state["runs"].as_object().unwrap().values() {
            one_of(
                run,
                "status",
                &["running", "completed", "failed", "interrupted"],
            )?;
            text(run, "taskId")?;
            if !run["threadIds"].is_array()
                || !run["worktrees"].is_array()
                || run["startedAt"].as_u64().is_none()
            {
                return Err("invalid scheduler store: malformed run links or startedAt".into());
            }
        }
        Ok(state)
    }

    fn save(&self, state: &Value) -> Result<()> {
        let parent = self.path.parent().unwrap();
        let temp = parent.join(format!(".schedules-{}.tmp", new_id("write")));
        let result = (|| -> Result<()> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temp)
                .map_err(|e| format!("create schedule store: {e}"))?;
            file.write_all(&serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?)
                .map_err(|e| format!("write schedule store: {e}"))?;
            file.sync_all()
                .map_err(|e| format!("sync schedule store: {e}"))?;
            drop(file);
            fs::rename(&temp, &self.path).map_err(|e| format!("replace schedule store: {e}"))?;
            #[cfg(unix)]
            File::open(parent)
                .and_then(|file| file.sync_all())
                .map_err(|e| format!("sync scheduler directory: {e}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }

    fn rpc(&self, method: &str, params: &Value, now: u64) -> Result<Value> {
        object(params)?;
        self.transaction(|state| match method {
            "scheduler/list" => {
                validate_list_filters(params)?;
                let mut data: Vec<Value> = state["tasks"].as_object().unwrap().values()
                    .filter(|task| ["state", "mode", "kind"].iter().all(|field| params.get(*field).is_none_or(|value| task[*field] == *value)))
                    .cloned().collect();
                data.sort_by(|a,b| b["updatedAt"].as_u64().cmp(&a["updatedAt"].as_u64()));
                data.truncate(limit(params)?);
                Ok((json!({"data":data}), false))
            }
            "scheduler/read" => Ok((json!({"task":lookup(state,"tasks",text(params,"id")?)?}), false)),
            "scheduler/create" => {
                let mut task = json!({"id":new_id("schedule"),"kind":"cron","mode":"codex","state":"active","reasoningEffort":"medium","notificationPolicy":null,"autoArchive":false,"executionEnvironment":"local","projects":[],"projectId":null,"targetThreadId":null,"createdAt":now,"updatedAt":now});
                patch_task(&mut task, params, false)?;
                let parsed = calendar(&task["schedule"])?;
                let first = first_occurrence(&parsed)?;
                // Reject an unbounded/unsafe catch-up scan before saving a job.
                next_after(&parsed, now)?;
                task["schedule"] = parsed.schedule;
                task["nextRunAt"] = json!(first);
                let id = task["id"].as_str().unwrap().to_string();
                state["tasks"][&id] = task.clone();
                Ok((json!({"task":task}), true))
            }
            "scheduler/update" => {
                let id = text(params,"id")?;
                let mut task = lookup(state,"tasks",id)?.clone();
                patch_task(&mut task, params, true)?;
                if params.get("schedule").is_some() {
                    let parsed = calendar(&task["schedule"])?;
                    next_after(&parsed, now)?;
                    task["nextRunAt"] = json!(first_occurrence(&parsed)?);
                    task["schedule"] = parsed.schedule;
                    state["cursors"].as_object_mut().unwrap().remove(id);
                    if task["state"] == "completed" { task["state"] = json!("active"); }
                } else if task["state"] == "active" && task["nextRunAt"].is_null() {
                    return Err("schedule is exhausted; provide a new schedule to activate it".into());
                }
                task["updatedAt"] = json!(now);
                state["tasks"][id] = task.clone();
                Ok((json!({"task":task}), true))
            }
            "scheduler/delete" => {
                let id = text(params,"id")?;
                lookup(state,"tasks",id)?;
                if !state["inFlight"][id].is_null() { return Err("stop or finish the running task before deleting it".into()); }
                let delete_history = match params.get("deleteHistory") { None=>false,Some(value)=>value.as_bool().ok_or("deleteHistory must be a boolean")? };
                state["tasks"].as_object_mut().unwrap().remove(id);
                state["cursors"].as_object_mut().unwrap().remove(id);
                if delete_history { state["runs"].as_object_mut().unwrap().retain(|_,run| run["taskId"] != id); }
                let retained = state["runs"].as_object().unwrap().values().filter(|run|run["taskId"]==id).count();
                Ok((json!({"id":id,"deleted":true,"retainedRuns":retained}), true))
            }
            "scheduler/runs" => {
                optional_text(params,"taskId")?;
                let mut data: Vec<_> = state["runs"].as_object().unwrap().values().filter(|run|params["taskId"].as_str().is_none_or(|id|run["taskId"]==id)).cloned().collect();
                data.sort_by(|a,b|b["startedAt"].as_u64().cmp(&a["startedAt"].as_u64()).then_with(||b["id"].as_str().cmp(&a["id"].as_str())));
                data.truncate(limit(params)?);
                Ok((json!({"data":data}), false))
            }
            "scheduler/run/update" => {
                let id = text(params,"id")?;
                let mut run = lookup(state,"runs",id)?.clone();
                patch_run(&mut run, params, now)?;
                state["runs"][id] = run.clone();
                Ok((json!({"run":run}), true))
            }
            _ => Err(format!("unknown scheduler method: {method}")),
        })
    }

    fn claim_due(&self, now: u64) -> Result<Vec<Value>> {
        timestamp(now)?;
        self.transaction(|state| {
            let mut due: Vec<(String, u64)> = state["tasks"]
                .as_object()
                .unwrap()
                .iter()
                .filter(|(id, task)| task["state"] == "active" && state["inFlight"][*id].is_null())
                .filter_map(|(id, task)| {
                    task["nextRunAt"]
                        .as_u64()
                        .filter(|time| *time <= now)
                        .map(|time| (id.clone(), time))
                })
                .collect();
            due.sort_by_key(|(_, at)| *at);
            let mut claims = Vec::new();
            for (id, at) in due {
                let mut task = state["tasks"][&id].clone();
                let original = calendar(&task["schedule"])?;
                let source = if state["cursors"][&id].is_null() {
                    original
                } else {
                    calendar(&state["cursors"][&id])?
                };
                let next = if source.recurring {
                    next_after(&source, now)?
                } else {
                    NextOccurrence {
                        next: None,
                        cursor: None,
                    }
                };
                task["nextRunAt"] = json!(next.next);
                if next.next.is_none() {
                    task["state"] = json!("completed");
                }
                task["updatedAt"] = json!(now);
                if let Some(cursor) = next.cursor {
                    state["cursors"][&id] = cursor;
                }
                state["tasks"][&id] = task.clone();
                claims.push(claim(state, task, at, now, "scheduled")?);
            }
            let changed = !claims.is_empty();
            Ok((claims, changed))
        })
    }

    fn claim_run(&self, id: &str, now: u64) -> Result<Value> {
        timestamp(now)?;
        self.transaction(|state| {
            let task = lookup(state, "tasks", id)?.clone();
            Ok((claim(state, task, now, now, "manual")?, true))
        })
    }

    fn update_run(&self, id: &str, patch: &Value, now: u64) -> Result<Value> {
        object(patch)?;
        self.transaction(|state| {
            let mut run = lookup(state, "runs", id)?.clone();
            patch_run(&mut run, patch, now)?;
            state["runs"][id] = run.clone();
            Ok((json!({"run":run}), true))
        })
    }

    fn finish_run(&self, id: &str, patch: &Value, now: u64) -> Result<Value> {
        object(patch)?;
        one_of(patch, "status", &["completed", "failed", "interrupted"])?;
        self.transaction(|state| {
            let mut run = lookup(state,"runs",id)?.clone();
            if run["status"] != "running" { return Err("run has already finished".into()); }
            let task_id = text(&run,"taskId")?.to_string();
            if state["inFlight"][&task_id] != id {return Err("run is not the task's in-flight execution".into());}
            patch_run_links(&mut run,patch)?;
            let finished = patch.get("completedAt").map(|v|v.as_u64().ok_or("completedAt must be a timestamp")).transpose()?.unwrap_or(now.max(run["startedAt"].as_u64().unwrap_or(now)));
            if finished < run["startedAt"].as_u64().unwrap_or(0) {return Err("completedAt must not precede startedAt".into());}
            timestamp(finished)?;
            for field in ["summary","error"] { if patch.get(field).is_some_and(|v|!v.is_null() && !v.is_string()) {return Err(format!("{field} must be a string or null"));} }
            if patch["status"]=="completed" && !patch["error"].is_null() {return Err("a successful run cannot have an error".into());}
            run["status"] = patch["status"].clone();
            run["completedAt"] = json!(finished);
            if patch.get("hasUpdate").is_some_and(|value|!value.is_boolean()) {return Err("hasUpdate must be a boolean".into());}
            run["summary"] = patch.get("summary").cloned().unwrap_or(Value::Null);
            run["hasUpdate"] = patch.get("hasUpdate").cloned().unwrap_or(Value::Null);
            run["error"] = if patch["status"]=="completed" {Value::Null} else {patch.get("error").filter(|v|!v.is_null()).cloned().unwrap_or_else(||json!("Run did not complete successfully"))};
            run["reviewState"] = json!("PENDING_REVIEW");
            run["unread"] = json!(true);
            run["readAt"] = Value::Null;
            let review_patch = json!({"unread":patch.get("unread").cloned().unwrap_or(json!(true)),"reviewState":patch.get("reviewState").cloned().unwrap_or(json!("PENDING_REVIEW"))});
            patch_run(&mut run,&review_patch,finished)?;
            state["runs"][id] = run.clone();
            state["inFlight"].as_object_mut().unwrap().remove(&task_id);
            Ok((json!({"run":run}),true))
        })
    }

    fn recover_interrupted(&self, now: u64) -> Result<()> {
        self.transaction(|state| {
            let mut changed = !state["inFlight"].as_object().unwrap().is_empty();
            for run in state["runs"].as_object_mut().unwrap().values_mut() {
                if run["status"] == "running" {
                    run["status"] = json!("interrupted");
                    run["completedAt"] = json!(now.max(run["startedAt"].as_u64().unwrap_or(now)));
                    run["error"] = json!("Scheduler restarted before this run finished");
                    run["reviewState"] = json!("PENDING_REVIEW");
                    run["unread"] = json!(true);
                    run["readAt"] = Value::Null;
                    changed = true;
                }
            }
            state["inFlight"] = json!({});
            Ok(((), changed))
        })
    }
}

fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "scheduler files must not be symlinks: {}",
            path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect scheduler file: {error}")),
    }
}
fn lookup<'a>(state: &'a Value, collection: &str, id: &str) -> Result<&'a Value> {
    state[collection]
        .get(id)
        .ok_or_else(|| format!("{collection} item not found: {id}"))
}
fn limit(params: &Value) -> Result<usize> {
    match params.get("limit") {
        None => Ok(100),
        Some(value) => value
            .as_u64()
            .filter(|v| *v > 0 && *v <= 1000)
            .map(|v| v as usize)
            .ok_or_else(|| "limit must be 1..1000".into()),
    }
}
fn validate_list_filters(params: &Value) -> Result<()> {
    for (field, allowed) in [
        ("state", vec!["active", "paused", "completed"]),
        ("mode", vec!["chat", "work", "codex"]),
        ("kind", vec!["cron", "heartbeat"]),
    ] {
        if params.get(field).is_some() {
            one_of(params, field, &allowed)?;
        }
    }
    Ok(())
}
fn patch_task(task: &mut Value, patch: &Value, updating: bool) -> Result<()> {
    for key in object(patch)?.keys() {
        if ![
            "id",
            "name",
            "prompt",
            "mode",
            "cwd",
            "projectId",
            "projects",
            "model",
            "reasoningEffort",
            "kind",
            "schedule",
            "state",
            "notificationPolicy",
            "autoArchive",
            "targetThreadId",
            "threadId",
            "executionEnvironment",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("unsupported task field: {key}"));
        }
    }
    for key in [
        "name",
        "prompt",
        "mode",
        "cwd",
        "projectId",
        "projects",
        "model",
        "reasoningEffort",
        "kind",
        "schedule",
        "state",
        "notificationPolicy",
        "autoArchive",
        "targetThreadId",
        "executionEnvironment",
    ] {
        if let Some(value) = patch.get(key) {
            task[key] = value.clone();
        }
    }
    if patch.get("targetThreadId").is_none() {
        if let Some(id) = patch.get("threadId") {
            task["targetThreadId"] = id.clone();
        }
    }
    if task["kind"] == "standalone" {
        task["kind"] = json!("cron");
    }
    for key in ["name", "prompt", "cwd", "model"] {
        text(task, key)?;
    }
    one_of(task, "mode", &["chat", "work", "codex"])?;
    one_of(task, "kind", &["cron", "heartbeat"])?;
    one_of(
        task,
        "state",
        if updating && patch.get("state").is_none() {
            &["active", "paused", "completed"]
        } else {
            &["active", "paused"]
        },
    )?;
    one_of(
        task,
        "reasoningEffort",
        &[
            "auto", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ],
    )?;
    one_of(task, "executionEnvironment", &["local", "worktree"])?;
    optional_text(task, "projectId")?;
    optional_text(task, "targetThreadId")?;
    if task["kind"] == "heartbeat" {
        text(task, "targetThreadId")?;
    }
    if !task["notificationPolicy"].is_null() && task["notificationPolicy"] != "failed_runs_only" {
        return Err("notificationPolicy must be null or failed_runs_only".into());
    }
    if !task["autoArchive"].is_boolean() {
        return Err("autoArchive must be a boolean".into());
    }
    if task["kind"] == "heartbeat" && task["autoArchive"] == true {
        return Err("heartbeat tasks cannot archive their target conversation".into());
    }
    let projects = task["projects"]
        .as_array()
        .ok_or("projects must be an array")?;
    for project in projects {
        object(project)?;
        text(project, "cwd")?;
        optional_text(project, "projectId")?;
        optional_text(project, "name")?;
    }
    Ok(())
}
fn claim(
    state: &mut Value,
    task: Value,
    scheduled_for: u64,
    now: u64,
    trigger: &str,
) -> Result<Value> {
    let task_id = text(&task, "id")?.to_string();
    if !state["inFlight"][&task_id].is_null() {
        return Err("this task already has a running execution".into());
    }
    let id = new_id("run");
    let run = json!({"id":id,"taskId":task_id,"taskName":task["name"],"kind":task["kind"],"mode":task["mode"],"trigger":trigger,"status":"running","scheduledFor":scheduled_for,"startedAt":now,"completedAt":null,"threadId":null,"threadIds":[],"worktrees":[],"error":null,"summary":null,"hasUpdate":null,"unread":false,"readAt":null,"reviewState":"IN_PROGRESS"});
    state["inFlight"][&task_id] = json!(id);
    state["runs"][&id] = run.clone();
    Ok(json!({"task":task,"run":run}))
}
fn patch_run_links(run: &mut Value, patch: &Value) -> Result<()> {
    optional_text(patch, "threadId")?;
    if let Some(ids) = patch.get("threadIds") {
        let ids = ids.as_array().ok_or("threadIds must be an array")?;
        if ids
            .iter()
            .any(|id| !id.as_str().is_some_and(|id| !id.trim().is_empty()))
        {
            return Err("threadIds must contain non-empty strings".into());
        }
        let mut unique = Vec::new();
        for id in ids {
            if !unique.contains(id) {
                unique.push(id.clone());
            }
        }
        run["threadIds"] = json!(unique);
        if patch.get("threadId").is_none() {
            run["threadId"] = unique.first().cloned().unwrap_or(Value::Null);
        }
    }
    if let Some(id) = patch.get("threadId") {
        run["threadId"] = id.clone();
        if !id.is_null() && !run["threadIds"].as_array().unwrap().contains(id) {
            run["threadIds"].as_array_mut().unwrap().push(id.clone());
        }
    }
    if let Some(worktrees) = patch.get("worktrees") {
        let items = worktrees.as_array().ok_or("worktrees must be an array")?;
        for item in items {
            if item.is_object() {
                text(item, "path")?;
            } else if !item.as_str().is_some_and(|s| !s.trim().is_empty()) {
                return Err("worktrees must contain paths or objects with a path".into());
            }
        }
        run["worktrees"] = worktrees.clone();
    }
    Ok(())
}
fn patch_run(run: &mut Value, patch: &Value, now: u64) -> Result<()> {
    for key in object(patch)?.keys() {
        if ![
            "id",
            "unread",
            "reviewState",
            "threadId",
            "threadIds",
            "worktrees",
        ]
        .contains(&key.as_str())
        {
            return Err(format!("run updates cannot change {key}"));
        }
    }
    patch_run_links(run, patch)?;
    if let Some(unread) = patch.get("unread") {
        if !unread.is_boolean() {
            return Err("unread must be a boolean".into());
        }
        run["unread"] = unread.clone();
        run["readAt"] = if unread == true {
            Value::Null
        } else {
            json!(now)
        };
    }
    if patch.get("reviewState").is_some() {
        one_of(
            patch,
            "reviewState",
            &["IN_PROGRESS", "PENDING_REVIEW", "ACCEPTED", "ARCHIVED"],
        )?;
        if (run["status"] == "running") != (patch["reviewState"] == "IN_PROGRESS") {
            return Err("IN_PROGRESS is reserved for running executions".into());
        }
        run["reviewState"] = patch["reviewState"].clone();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct Fixture {
        root: PathBuf,
        store: Store,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(new_id("mc-scheduler-test"));
            fs::create_dir_all(&root).unwrap();
            Self {
                store: Store {
                    path: root.join("schedules.json"),
                },
                root,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Ok(mut owners) = DISPATCH_OWNERS.lock() {
                if let (Some(owners), Ok(key)) = (owners.as_mut(), self.root.canonicalize()) {
                    owners.remove(&key);
                }
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn ms(iso: &str) -> u64 {
        DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .timestamp_millis() as u64
    }
    fn schedule(start: &str, rule: Option<&str>, zone: &str) -> Value {
        json!({"dtstart":start,"rrule":rule,"timezone":zone})
    }
    fn task(schedule: Value) -> Value {
        json!({"name":"scheduled work","prompt":"check the project","mode":"codex","cwd":"/tmp","model":"local-mock-model","schedule":schedule})
    }
    fn create(store: &Store, params: Value, now: u64) -> Value {
        store.rpc("scheduler/create", &params, now).unwrap()["task"].clone()
    }

    #[test]
    fn schedules_preserve_auto_and_explicit_model_efforts() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T00:00:00Z");
        for effort in ["auto", "none", "minimal", "max", "ultra"] {
            let mut params = task(schedule(
                "DTSTART:20260102T090000",
                Some("FREQ=DAILY;COUNT=1"),
                "UTC",
            ));
            params["reasoningEffort"] = json!(effort);
            let created = create(&fixture.store, params, now);
            assert_eq!(created["reasoningEffort"], effort);
        }
    }

    #[test]
    fn private_atomic_storage_persists_projects_and_rejects_bad_updates_without_reset() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T00:00:00Z");
        let mut params = task(schedule(
            "DTSTART:20260102T090000",
            Some("FREQ=DAILY;COUNT=3"),
            "Asia/Shanghai",
        ));
        params["kind"] = json!("standalone");
        params["state"] = json!("paused");
        params["executionEnvironment"] = json!("worktree");
        params["projects"] =
            json!([{"cwd":"/tmp/a","projectId":"project-a","name":"A"},{"cwd":"/tmp/b"}]);
        let created = create(&fixture.store, params, now);
        let id = created["id"].as_str().unwrap();
        assert_eq!(created["kind"], "cron");
        assert_eq!(
            created["schedule"]["dtstart"],
            "DTSTART;TZID=Asia/Shanghai:20260102T090000"
        );
        assert!(created["notificationPolicy"].is_null());
        let reopened = Store {
            path: fixture.store.path.clone(),
        };
        assert_eq!(
            reopened
                .rpc("scheduler/read", &json!({"id":id}), now)
                .unwrap()["task"],
            created
        );
        assert!(reopened
            .rpc(
                "scheduler/update",
                &json!({"id":id,"notificationPolicy":"all"}),
                now
            )
            .is_err());
        assert_eq!(
            reopened
                .rpc("scheduler/read", &json!({"id":id}), now)
                .unwrap()["task"],
            created
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&fixture.store.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(fixture.root.join("schedules.lock"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(!fs::read_dir(&fixture.root).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        fs::write(&fixture.store.path, b"broken-json").unwrap();
        assert!(reopened.rpc("scheduler/list", &json!({}), now).is_err());
        assert_eq!(fs::read(&fixture.store.path).unwrap(), b"broken-json");
    }

    #[test]
    fn missed_runs_merge_and_calendar_completion_is_independent_of_run_success() {
        let fixture = Fixture::new();
        let now = ms("2026-01-03T12:00:00Z");
        let created = create(
            &fixture.store,
            task(schedule(
                "DTSTART:20260101T090000Z",
                Some("FREQ=DAILY;COUNT=4"),
                "UTC",
            )),
            now,
        );
        let id = created["id"].as_str().unwrap();
        let claims = fixture.store.claim_due(now).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["run"]["status"], "running");
        assert_eq!(claims[0]["run"]["scheduledFor"], ms("2026-01-01T09:00:00Z"));
        assert_eq!(claims[0]["task"]["nextRunAt"], ms("2026-01-04T09:00:00Z"));
        assert!(fixture.store.claim_due(now).unwrap().is_empty());
        let run = claims[0]["run"]["id"].as_str().unwrap();
        fixture
            .store
            .finish_run(
                run,
                &json!({"status":"failed","error":"execution failed"}),
                now + 1,
            )
            .unwrap();
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/read", &json!({"id":id}), now)
                .unwrap()["task"]["state"],
            "active"
        );
        let claims = fixture.store.claim_due(ms("2026-01-04T09:00:00Z")).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["task"]["state"], "completed");
        assert_eq!(claims[0]["run"]["status"], "running");
        let run = claims[0]["run"]["id"].as_str().unwrap();
        fixture
            .store
            .finish_run(
                run,
                &json!({"status":"failed","error":"still failed"}),
                ms("2026-01-04T09:01:00Z"),
            )
            .unwrap();
        assert!(fixture
            .store
            .claim_due(ms("2027-01-04T09:00:00Z"))
            .unwrap()
            .is_empty());
        let history = fixture
            .store
            .rpc("scheduler/runs", &json!({"taskId":id}), now)
            .unwrap();
        assert_eq!(history["data"].as_array().unwrap().len(), 2);
        assert!(history["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["status"] == "failed"));
    }

    #[test]
    fn manual_runs_preserve_pause_and_review_updates_cannot_fake_success() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T00:00:00Z");
        let mut params = task(schedule("DTSTART:20260102T090000Z", None, "UTC"));
        params["state"] = json!("paused");
        let created = create(&fixture.store, params, now);
        let id = created["id"].as_str().unwrap();
        let claimed = fixture.store.claim_run(id, now).unwrap();
        let run = claimed["run"]["id"].as_str().unwrap();
        assert!(fixture.store.claim_run(id, now).is_err());
        assert!(fixture
            .store
            .rpc("scheduler/delete", &json!({"id":id}), now)
            .is_err());
        assert!(fixture
            .store
            .update_run(run, &json!({"status":"completed"}), now)
            .is_err());
        assert!(fixture
            .store
            .finish_run(run, &json!({"summary":"not done yet"}), now)
            .is_err());
        fixture
            .store
            .rpc(
                "scheduler/update",
                &json!({"id":id,"state":"paused","name":"paused during execution"}),
                now,
            )
            .unwrap();
        fixture.store.update_run(run,&json!({"threadIds":["thread-a","thread-b"],"worktrees":[{"path":"/tmp/worktree-a","branch":"codex/automation"}]}),now).unwrap();
        let finished = fixture
            .store
            .finish_run(
                run,
                &json!({"status":"completed","summary":"verified completion"}),
                now + 1,
            )
            .unwrap();
        assert_eq!(
            finished["run"]["threadIds"],
            json!(["thread-a", "thread-b"])
        );
        assert_eq!(finished["run"]["reviewState"], "PENDING_REVIEW");
        assert_eq!(finished["run"]["unread"], true);
        assert!(fixture
            .store
            .finish_run(run, &json!({"status":"completed"}), now + 2)
            .is_err());
        let read = fixture
            .store
            .update_run(
                run,
                &json!({"unread":false,"reviewState":"ARCHIVED"}),
                now + 2,
            )
            .unwrap();
        assert_eq!(read["run"]["readAt"], now + 2);
        let restored = fixture
            .store
            .rpc(
                "scheduler/run/update",
                &json!({"id":run,"unread":true,"reviewState":"PENDING_REVIEW"}),
                now + 3,
            )
            .unwrap();
        assert!(restored["run"]["readAt"].is_null());
        let saved = fixture
            .store
            .rpc("scheduler/read", &json!({"id":id}), now)
            .unwrap();
        assert_eq!(saved["task"]["state"], "paused");
        assert_eq!(saved["task"]["nextRunAt"], created["nextRunAt"]);
        let deleted = fixture
            .store
            .rpc("scheduler/delete", &json!({"id":id}), now)
            .unwrap();
        assert_eq!(deleted["retainedRuns"], 1);
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/runs", &json!({"taskId":id}), now)
                .unwrap()["data"][0]["taskName"],
            "scheduled work"
        );
    }

    #[test]
    fn restart_marks_running_records_interrupted_and_releases_inflight() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        let created = create(
            &fixture.store,
            task(schedule("DTSTART:20260101T090000Z", None, "UTC")),
            now,
        );
        let id = created["id"].as_str().unwrap();
        let first = fixture.store.claim_due(now).unwrap().remove(0);
        let run = first["run"]["id"].as_str().unwrap();
        fixture
            .store
            .update_run(run, &json!({"threadId":"thread-before-crash"}), now)
            .unwrap();
        let reopened = Store {
            path: fixture.store.path.clone(),
        };
        reopened.recover_interrupted(now + 1000).unwrap();
        let history = reopened
            .rpc("scheduler/runs", &json!({"taskId":id}), now)
            .unwrap();
        assert_eq!(history["data"][0]["status"], "interrupted");
        assert_eq!(
            history["data"][0]["threadIds"],
            json!(["thread-before-crash"])
        );
        assert_eq!(history["data"][0]["unread"], true);
        assert!(history["data"][0]["error"]
            .as_str()
            .unwrap()
            .contains("restarted"));
        let next = reopened.claim_run(id, now + 2000).unwrap();
        assert_ne!(next["run"]["id"], first["run"]["id"]);
        assert_eq!(next["task"]["state"], "completed");
    }

    #[test]
    fn timezone_dst_changes_utc_offset_while_preserving_local_time() {
        for (start, first, second, hours) in [
            (
                "DTSTART:20260307T090000",
                "2026-03-07T14:00:00Z",
                "2026-03-08T13:00:00Z",
                23,
            ),
            (
                "DTSTART:20261031T090000",
                "2026-10-31T13:00:00Z",
                "2026-11-01T14:00:00Z",
                25,
            ),
        ] {
            let parsed = calendar(&schedule(
                start,
                Some("FREQ=DAILY;COUNT=2"),
                "America/New_York",
            ))
            .unwrap();
            assert_eq!(first_occurrence(&parsed).unwrap(), ms(first));
            let next = next_after(&parsed, ms(first)).unwrap();
            assert_eq!(next.next, Some(ms(second)));
            assert_eq!(ms(second) - ms(first), hours * 3_600_000);
        }
        let converted = calendar(&schedule(
            "DTSTART:20260307T140000Z",
            Some("FREQ=DAILY;COUNT=2"),
            "America/New_York",
        ))
        .unwrap();
        assert_eq!(
            converted.schedule["dtstart"],
            "DTSTART;TZID=America/New_York:20260307T090000"
        );
        assert_eq!(
            next_after(&converted, ms("2026-03-07T14:00:00Z"))
                .unwrap()
                .next,
            Some(ms("2026-03-08T13:00:00Z"))
        );
    }

    #[test]
    fn cursor_preserves_byday_interval_count_and_monthly_positions() {
        for rule in [
            "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=8",
            "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=6",
            "FREQ=YEARLY;INTERVAL=2;BYWEEKNO=1;BYDAY=MO;COUNT=4",
        ] {
            let original = calendar(&schedule(
                "DTSTART:20260105T090000",
                Some(rule),
                "America/New_York",
            ))
            .unwrap();
            let expected = occurrence_batch(&original, 32).unwrap().dates;
            let mut current = original;
            for (index, date) in expected.iter().enumerate() {
                let found = next_after(&current, millis(date).unwrap()).unwrap();
                assert_eq!(
                    found.next,
                    expected.get(index + 1).map(|date| millis(date).unwrap()),
                    "rule={rule} at {date}"
                );
                if let Some(cursor) = found.cursor {
                    current = calendar(&cursor).unwrap();
                }
            }
        }
    }

    #[test]
    fn until_is_honored_and_invalid_dense_or_limited_rules_are_errors() {
        let parsed = calendar(&schedule(
            "DTSTART:20260101T090000Z",
            Some("FREQ=DAILY;UNTIL=20260103T090000Z"),
            "UTC",
        ))
        .unwrap();
        assert_eq!(
            next_after(&parsed, ms("2026-01-02T09:00:00Z"))
                .unwrap()
                .next,
            Some(ms("2026-01-03T09:00:00Z"))
        );
        assert!(next_after(&parsed, ms("2026-01-03T09:00:00Z"))
            .unwrap()
            .next
            .is_none());
        for rule in [
            "FREQ=SECONDLY",
            "FREQ=MINUTELY;BYSECOND=0,1",
            "FREQ=DAILY;COUNT=2;UNTIL=20260103T090000Z",
            "FREQ=NOTREAL",
            "FREQ=DAILY;INTERVAL=0",
            "FREQ=DAILY;COUNT=0",
            "FREQ=DAILY;FREQ=WEEKLY",
        ] {
            assert!(
                calendar(&schedule("DTSTART:20260101T090000Z", Some(rule), "UTC")).is_err(),
                "{rule}"
            );
        }
        assert!(calendar(&schedule(
            "DTSTART:20260101T090000",
            None,
            "Unknown/Timezone"
        ))
        .is_err());
        assert!(calendar(&schedule(
            "DTSTART;TZID=UTC:20260101T090000",
            None,
            "Asia/Shanghai"
        ))
        .is_err());
        let impossible = calendar(&schedule(
            "DTSTART:20260101T090000Z",
            Some("FREQ=DAILY;BYMONTH=2;BYMONTHDAY=30"),
            "UTC",
        ))
        .unwrap();
        assert!(first_occurrence(&impossible).is_err());
        let ancient = calendar(&schedule(
            "DTSTART:20000101T090000Z",
            Some("FREQ=MINUTELY"),
            "UTC",
        ))
        .unwrap();
        assert!(next_after(&ancient, ms("2026-01-01T09:00:00Z"))
            .unwrap_err()
            .contains("too many missed"));
    }

    #[test]
    fn heartbeat_requires_a_target_and_cannot_archive_its_main_conversation() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        let mut params = task(schedule("DTSTART:20260102T090000Z", None, "UTC"));
        params["kind"] = json!("heartbeat");
        assert!(fixture.store.rpc("scheduler/create", &params, now).is_err());
        params["threadId"] = json!("existing-thread");
        params["autoArchive"] = json!(true);
        assert!(fixture.store.rpc("scheduler/create", &params, now).is_err());
        params["autoArchive"] = json!(false);
        params["notificationPolicy"] = json!("failed_runs_only");
        let created = create(&fixture.store, params, now);
        assert_eq!(created["targetThreadId"], "existing-thread");
        assert!(created.get("threadId").is_none());
    }

    #[test]
    fn concurrent_claims_execute_each_due_task_only_once() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        create(
            &fixture.store,
            task(schedule(
                "DTSTART:20260101T090000Z",
                Some("FREQ=DAILY"),
                "UTC",
            )),
            now,
        );
        let barrier = Arc::new(Barrier::new(12));
        let workers: Vec<_> = (0..12)
            .map(|_| {
                let store = fixture.store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store.claim_due(now).unwrap().len()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>(),
            1
        );
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/runs", &json!({}), now)
                .unwrap()["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn delete_history_is_explicit_and_read_updates_do_not_change_execution_status() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        let created = create(
            &fixture.store,
            task(schedule("DTSTART:20260102T090000Z", None, "UTC")),
            now,
        );
        let id = created["id"].as_str().unwrap();
        let run = fixture.store.claim_run(id, now).unwrap();
        let run_id = run["run"]["id"].as_str().unwrap();
        assert!(fixture
            .store
            .update_run(run_id, &json!({"reviewState":"ACCEPTED"}), now)
            .is_err());
        fixture
            .store
            .finish_run(
                run_id,
                &json!({"status":"interrupted","error":"user stopped it"}),
                now + 1,
            )
            .unwrap();
        let updated = fixture
            .store
            .update_run(
                run_id,
                &json!({"reviewState":"ACCEPTED","unread":false}),
                now + 2,
            )
            .unwrap();
        assert_eq!(updated["run"]["status"], "interrupted");
        fixture
            .store
            .rpc(
                "scheduler/delete",
                &json!({"id":id,"deleteHistory":true}),
                now + 3,
            )
            .unwrap();
        assert!(fixture
            .store
            .rpc("scheduler/runs", &json!({"taskId":id}), now + 4)
            .unwrap()["data"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    fn worker(store: &Store, now: u64, action: &str) -> std::process::Child {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "scheduler::tests::process_worker", "--ignored"])
            .env("MCODEX_SCHEDULER_TEST_PATH", &store.path)
            .env("MCODEX_SCHEDULER_TEST_NOW", now.to_string())
            .env("MCODEX_SCHEDULER_TEST_ACTION", action)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn wait_worker(mut child: std::process::Child) {
        let started = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() > Duration::from_secs(5) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("scheduler test subprocess exceeded five seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "subprocess helper; invoked only with isolated fixture environment"]
    fn process_worker() {
        let Ok(path) = std::env::var("MCODEX_SCHEDULER_TEST_PATH") else {
            return;
        };
        let now = std::env::var("MCODEX_SCHEDULER_TEST_NOW")
            .unwrap()
            .parse()
            .unwrap();
        let store = Store {
            path: PathBuf::from(path),
        };
        match std::env::var("MCODEX_SCHEDULER_TEST_ACTION")
            .unwrap()
            .as_str()
        {
            "claim" => {
                store.claim_due(now).unwrap();
            }
            "owner-busy" => assert!(ensure_dispatch_owner(&store, now)
                .unwrap_err()
                .contains("another process")),
            other => panic!("unknown scheduler test action {other}"),
        }
    }

    #[test]
    fn cross_process_claim_is_atomic() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        create(
            &fixture.store,
            task(schedule(
                "DTSTART:20260101T090000Z",
                Some("FREQ=DAILY"),
                "UTC",
            )),
            now,
        );
        let children: Vec<_> = (0..4)
            .map(|_| worker(&fixture.store, now, "claim"))
            .collect();
        for child in children {
            wait_worker(child);
        }
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/runs", &json!({}), now)
                .unwrap()["data"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn live_owner_cannot_be_recovered_or_replaced_by_a_second_process() {
        let fixture = Fixture::new();
        let now = ms("2026-01-01T09:00:00Z");
        ensure_dispatch_owner(&fixture.store, now).unwrap();
        create(
            &fixture.store,
            task(schedule("DTSTART:20260101T090000Z", None, "UTC")),
            now,
        );
        fixture.store.claim_due(now).unwrap();
        ensure_dispatch_owner(&fixture.store, now + 1).unwrap();
        wait_worker(worker(&fixture.store, now + 1, "owner-busy"));
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/runs", &json!({}), now)
                .unwrap()["data"][0]["status"],
            "running"
        );
    }

    fn assert_dates_and_cursors(mut parsed: Calendar, expected: &[&str]) {
        let expected: Vec<u64> = expected.iter().map(|date| ms(date)).collect();
        assert_eq!(
            occurrence_batch(&parsed, 128)
                .unwrap()
                .dates
                .iter()
                .map(|date| millis(date).unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(first_occurrence(&parsed).unwrap(), expected[0]);
        for (index, date) in expected.iter().enumerate() {
            let next = next_after(&parsed, *date).unwrap();
            assert_eq!(next.next, expected.get(index + 1).copied());
            if let Some(cursor) = next.cursor {
                parsed = calendar(&cursor).unwrap();
            }
        }
    }

    #[test]
    fn dst_gap_is_skipped_and_never_consumes_count_including_a_gap_dtstart() {
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20260307T023000",
                Some("FREQ=DAILY;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-03-07T07:30:00Z",
                "2026-03-09T06:30:00Z",
                "2026-03-10T06:30:00Z",
            ],
        );
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20260308T023000",
                Some("FREQ=DAILY;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-03-09T06:30:00Z",
                "2026-03-10T06:30:00Z",
                "2026-03-11T06:30:00Z",
            ],
        );
        // COUNT=3 must survive sixty consecutive nonexistent minute candidates.
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20260308T020000",
                Some("FREQ=MINUTELY;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-03-08T07:00:00Z",
                "2026-03-08T07:01:00Z",
                "2026-03-08T07:02:00Z",
            ],
        );
    }

    #[test]
    fn dst_gap_honors_byday_and_multiple_byhour_without_normalized_duplicates() {
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20260301T023000",
                Some("FREQ=WEEKLY;BYDAY=SU;BYHOUR=2;BYMINUTE=30;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-03-01T07:30:00Z",
                "2026-03-15T06:30:00Z",
                "2026-03-22T06:30:00Z",
            ],
        );
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20260308T000000",
                Some("FREQ=DAILY;BYHOUR=2,3;BYMINUTE=30;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-03-08T07:30:00Z",
                "2026-03-09T06:30:00Z",
                "2026-03-09T07:30:00Z",
            ],
        );
    }

    #[test]
    fn autumn_fold_selects_first_local_occurrence_once_and_compares_until_in_utc() {
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20261031T013000",
                Some("FREQ=DAILY;COUNT=3"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-10-31T05:30:00Z",
                "2026-11-01T05:30:00Z",
                "2026-11-02T06:30:00Z",
            ],
        );
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20261101T003000",
                Some("FREQ=HOURLY;COUNT=4"),
                "America/New_York",
            ))
            .unwrap(),
            &[
                "2026-11-01T04:30:00Z",
                "2026-11-01T05:30:00Z",
                "2026-11-01T07:30:00Z",
                "2026-11-01T08:30:00Z",
            ],
        );
        // 06:15Z is the second 01:15. The first 01:30 (05:30Z) is still before it.
        assert_dates_and_cursors(
            calendar(&schedule(
                "DTSTART:20261101T003000",
                Some("FREQ=HOURLY;UNTIL=20261101T061500Z"),
                "America/New_York",
            ))
            .unwrap(),
            &["2026-11-01T04:30:00Z", "2026-11-01T05:30:00Z"],
        );
    }

    #[test]
    fn persisted_claims_skip_gap_keep_count_and_preserve_verified_has_update() {
        let fixture = Fixture::new();
        let before = ms("2026-03-07T00:00:00Z");
        let created = create(
            &fixture.store,
            task(schedule(
                "DTSTART:20260307T023000",
                Some("FREQ=DAILY;COUNT=3"),
                "America/New_York",
            )),
            before,
        );
        let id = created["id"].as_str().unwrap();
        for (date, next) in [
            ("2026-03-07T07:30:00Z", Some(ms("2026-03-09T06:30:00Z"))),
            ("2026-03-09T06:30:00Z", Some(ms("2026-03-10T06:30:00Z"))),
            ("2026-03-10T06:30:00Z", None),
        ] {
            let now = ms(date);
            let claims = fixture.store.claim_due(now).unwrap();
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0]["task"]["nextRunAt"], json!(next));
            let run = claims[0]["run"]["id"].as_str().unwrap();
            assert!(fixture
                .store
                .update_run(run, &json!({"hasUpdate":true}), now)
                .is_err());
            let finished=fixture.store.finish_run(run,&json!({"status":"completed","hasUpdate":false,"summary":"No update from this run"}),now+1).unwrap();
            assert_eq!(finished["run"]["hasUpdate"], false);
            if date == "2026-03-07T07:30:00Z" {
                assert!(fixture
                    .store
                    .claim_due(ms("2026-03-08T07:30:00Z"))
                    .unwrap()
                    .is_empty());
            }
        }
        assert_eq!(
            fixture
                .store
                .rpc("scheduler/read", &json!({"id":id}), before)
                .unwrap()["task"]["state"],
            "completed"
        );
        let history = fixture
            .store
            .rpc("scheduler/runs", &json!({"taskId":id}), before)
            .unwrap();
        assert_eq!(history["data"].as_array().unwrap().len(), 3);
        assert!(history["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|run| run["hasUpdate"] == false));
    }
}
