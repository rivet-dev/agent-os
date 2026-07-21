//! Cron scheduling + the `CronManager`.
//!
//! Ported from `packages/core/src/cron/`. The `schedule` is a 5/6/7-field cron expression (croner
//! grammar) or an ISO-8601 one-shot timestamp. `CronAction::Callback` is in-process only
//! (non-serializable). `on_cron_event` returns NO unsubscribe in TS; the Rust equivalent is a
//! [`tokio::sync::broadcast::Receiver`] whose drop is the unsubscribe.
//!
//! Timing is owned by the [`ScheduleDriver`] (mirroring TS `CronManager.schedule` delegating to
//! `this.driver.schedule({...})`). The default [`crate::config::TimerScheduleDriver`] parses the
//! schedule, arms the timer, reschedules cron after each fire, and tears down on dispose. The manager
//! itself only registers job state and runs `execute_job` when the driver fires the callback.
//!
//! Cron fields are interpreted in the host LOCAL timezone, matching croner's default behavior.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, SecondsFormat, Timelike, Utc, Weekday,
};
use scc::HashMap as SccHashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::agent_os::AgentOs;
use crate::config::{ScheduleDriver, ScheduleEntry, ScheduleHandle};
use crate::error::ClientError;
use crate::session::{McpServerConfig, OpenSessionInput, PermissionPolicy, PromptInput};

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Overlap policy for a cron job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CronOverlap {
    #[default]
    Allow,
    Skip,
    Queue,
}

/// A cron action. `Callback` holds an in-process closure and cannot cross the wire.
#[derive(Clone)]
pub enum CronAction {
    /// Open a fresh durable session, prompt it, then delete it.
    Session {
        agent_type: String,
        prompt: String,
        options: Option<CronSessionOptions>,
    },
    /// Run a command via `exec`.
    Exec { command: String, args: Vec<String> },
    /// Invoke a host-side callback.
    Callback {
        #[allow(clippy::type_complexity)]
        callback: Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>,
    },
}

/// Durable session options accepted by a cron action. The action owns the
/// agent name and generates a unique session ID for each run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronSessionOptions {
    pub cwd: Option<String>,
    pub additional_directories: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    pub permission_policy: Option<PermissionPolicy>,
    pub skip_os_instructions: Option<bool>,
    pub additional_instructions: Option<String>,
}

/// Serializable description of a scheduled action. Callback jobs deliberately
/// expose only their kind: the host closure is execution state, not job data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CronActionInfo {
    Session {
        #[serde(rename = "agentType")]
        agent_type: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<CronSessionOptions>,
    },
    Exec {
        command: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
    },
    Callback,
}

impl From<&CronAction> for CronActionInfo {
    fn from(action: &CronAction) -> Self {
        match action {
            CronAction::Session {
                agent_type,
                prompt,
                options,
            } => Self::Session {
                agent_type: agent_type.clone(),
                prompt: prompt.clone(),
                options: options.clone(),
            },
            CronAction::Exec { command, args } => Self::Exec {
                command: command.clone(),
                args: args.clone(),
            },
            CronAction::Callback { .. } => Self::Callback,
        }
    }
}

impl std::fmt::Debug for CronAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CronAction::Session {
                agent_type, prompt, ..
            } => f
                .debug_struct("Session")
                .field("agent_type", agent_type)
                .field("prompt", prompt)
                .finish_non_exhaustive(),
            CronAction::Exec { command, args } => f
                .debug_struct("Exec")
                .field("command", command)
                .field("args", args)
                .finish(),
            CronAction::Callback { .. } => f.debug_struct("Callback").finish_non_exhaustive(),
        }
    }
}

/// Options for `schedule_cron`.
#[derive(Clone)]
pub struct CronJobOptions {
    /// Default: a fresh UUID.
    pub id: Option<String>,
    /// 5/6/7-field cron expression OR an ISO-8601 one-shot timestamp.
    pub schedule: String,
    pub action: CronAction,
    /// Default: [`CronOverlap::Allow`].
    pub overlap: Option<CronOverlap>,
}

/// Snapshot info for a cron job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJobInfo {
    pub id: String,
    pub schedule: String,
    pub action: CronActionInfo,
    pub overlap: CronOverlap,
    pub last_run: Option<String>,
    pub next_run: Option<String>,
    pub run_count: u64,
    pub running: bool,
}

/// A cron event emitted on each run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CronEvent {
    #[serde(rename = "cron:fire", rename_all = "camelCase")]
    Fire { job_id: String, time: String },
    #[serde(rename = "cron:complete", rename_all = "camelCase")]
    Complete {
        job_id: String,
        time: String,
        duration_ms: f64,
    },
    #[serde(rename = "cron:error", rename_all = "camelCase")]
    Error {
        job_id: String,
        time: String,
        error: String,
    },
}

/// Handle to a scheduled cron job. Dropping or calling [`CronJobHandle::cancel`] cancels it.
#[derive(Clone)]
pub struct CronJobHandle {
    pub id: String,
    pub(crate) manager: Arc<CronManager>,
}

impl CronJobHandle {
    /// Cancel the job (no-op if already cancelled/unknown).
    pub fn cancel(&self) {
        self.manager.cancel_job(&self.id);
    }
}

// ---------------------------------------------------------------------------
// CronManager + CronJobState
// ---------------------------------------------------------------------------

/// Internal per-job state.
pub(crate) struct CronJobState {
    pub schedule: String,
    pub action: CronAction,
    pub overlap: CronOverlap,
    pub last_run: parking_lot::Mutex<Option<DateTime<Utc>>>,
    pub next_run: parking_lot::Mutex<Option<DateTime<Utc>>>,
    pub run_count: std::sync::atomic::AtomicU64,
    pub running: AtomicBool,
    /// Set when a `Queue`-policy fire arrives while the job is already running; drained to exactly
    /// one deferred run when the active run completes. Mirrors TS `CronJobState.queued`.
    pub queued: AtomicBool,
    /// Driver-returned timer handle. Used by `cancel`/`dispose` to tear down the armed timer through
    /// the driver, mirroring TS `this.driver.cancel(state.handle)`.
    pub handle: ScheduleHandle,
}

/// Owns scheduled jobs, the schedule driver, and the cron event broadcast.
pub struct CronManager {
    pub(crate) jobs: SccHashMap<String, CronJobState>,
    pub(crate) schedule_lock: parking_lot::Mutex<()>,
    pub(crate) driver: Arc<dyn ScheduleDriver>,
    pub(crate) event_tx: broadcast::Sender<CronEvent>,
}

impl CronManager {
    /// Create a cron manager with the given schedule driver.
    pub(crate) fn new(driver: Arc<dyn ScheduleDriver>) -> Self {
        let (event_tx, _rx) = broadcast::channel(256);
        Self {
            jobs: SccHashMap::new(),
            schedule_lock: parking_lot::Mutex::new(()),
            driver,
            event_tx,
        }
    }

    /// Cancel a job by id (no-op if unknown).
    ///
    /// Mirrors TS `CronManager.cancel`: cancel the driver-armed timer (`this.driver.cancel(handle)`)
    /// and remove the job from the registry.
    pub(crate) fn cancel_job(&self, id: &str) {
        let _guard = self.schedule_lock.lock();
        if let Some((_, state)) = self.jobs.remove(id) {
            self.driver.cancel(&state.handle);
        }
    }

    /// Dispose all jobs (called during shutdown).
    ///
    /// Mirrors TS `CronManager.dispose`: cancel every armed timer through the driver, clear the
    /// registry, then call `this.driver.dispose()` to tear down all driver-held timer state.
    pub(crate) fn dispose(&self) {
        let _guard = self.schedule_lock.lock();
        self.jobs.scan(|_, state| {
            self.driver.cancel(&state.handle);
        });
        self.jobs.clear();
        self.driver.dispose();
    }
}

/// Execute a single job run, honoring the overlap policy. Emits `Fire`, then `Complete` or `Error`.
/// Re-runs once at the end if a `Queue`-policy run was deferred while busy.
///
/// Mirrors TS `CronManager.executeJob`. Handler/action errors never crash the manager; on error a
/// `cron:error` event is emitted instead of a `cron:complete`. Returns an explicitly boxed `Send`
/// future (rather than an `async fn`) so the recursive queued re-run does not form a
/// self-referential async auto-trait inference cycle that would defeat the `Send` bound required by
/// [`tokio::spawn`].
fn execute_job(
    manager: Arc<CronManager>,
    vm: AgentOs,
    id: String,
) -> futures::future::BoxFuture<'static, ()> {
    Box::pin(execute_job_inner(manager, vm, id))
}

async fn execute_job_inner(manager: Arc<CronManager>, vm: AgentOs, id: String) {
    let manager = &manager;
    let vm = &vm;
    let id = id.as_str();
    // Overlap policy: a running job either allows a concurrent run, skips this fire, or queues
    // exactly one deferred run.
    {
        let mut should_return = false;
        let mut should_queue = false;
        manager.jobs.read(id, |_, state| {
            if state.running.load(Ordering::SeqCst) {
                match state.overlap {
                    CronOverlap::Allow => {}
                    CronOverlap::Skip => should_return = true,
                    CronOverlap::Queue => should_queue = true,
                }
            }
        });
        if should_return {
            return;
        }
        if should_queue {
            manager.jobs.read(id, |_, state| {
                state.queued.store(true, Ordering::SeqCst);
            });
            return;
        }
    }

    // Mark running, record this run, and snapshot the action to dispatch.
    let action = match manager.jobs.read(id, |_, state| {
        state.running.store(true, Ordering::SeqCst);
        *state.last_run.lock() = Some(Utc::now());
        state.run_count.fetch_add(1, Ordering::SeqCst);
        state.action.clone()
    }) {
        Some(action) => action,
        None => return,
    };

    let _ = manager.event_tx.send(CronEvent::Fire {
        job_id: id.to_string(),
        time: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    });

    // TS `durationMs = Date.now() - startTime`, an integer millisecond count.
    let start = Utc::now();
    let result = run_action(vm, &action).await;
    let duration_ms = (Utc::now() - start).num_milliseconds() as f64;

    match result {
        Ok(()) => {
            let _ = manager.event_tx.send(CronEvent::Complete {
                job_id: id.to_string(),
                time: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                duration_ms,
            });
        }
        Err(error) => {
            let _ = manager.event_tx.send(CronEvent::Error {
                job_id: id.to_string(),
                time: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                error: error.to_string(),
            });
        }
    }

    // Clear running, recompute the next run, and drain a queued run if one was deferred.
    let mut run_queued = false;
    manager.jobs.read(id, |_, state| {
        state.running.store(false, Ordering::SeqCst);
        *state.next_run.lock() = compute_next_time(&state.schedule, Utc::now());
        if state.queued.swap(false, Ordering::SeqCst) {
            run_queued = true;
        }
    });

    if run_queued {
        let manager = Arc::clone(manager);
        let vm = vm.clone();
        let id = id.to_string();
        tokio::spawn(execute_job(manager, vm, id));
    }
}

/// Dispatch a [`CronAction`]. Mirrors TS `CronManager.runAction`.
///
/// `Session` opens a session, prompts it, and always deletes it (even if the prompt errors, the
/// delete still runs, matching the TS `finally`). `Exec` sends the structured `(command, args)` argv
/// verbatim via [`AgentOs::exec_argv`] (no string flattening / re-parsing). `Callback` awaits the
/// in-process future.
async fn run_action(vm: &AgentOs, action: &CronAction) -> Result<(), ClientError> {
    match action {
        CronAction::Session {
            agent_type,
            prompt,
            options,
        } => {
            let options = options.clone().unwrap_or_default();
            let session_id = format!("cron-{}", uuid::Uuid::new_v4());
            vm.open_session(OpenSessionInput {
                session_id: Some(session_id.clone()),
                agent: agent_type.clone(),
                cwd: options.cwd,
                additional_directories: options.additional_directories,
                env: options.env,
                mcp_servers: options.mcp_servers,
                permission_policy: options.permission_policy,
                skip_os_instructions: options.skip_os_instructions,
                additional_instructions: options.additional_instructions,
            })
            .await
            .map_err(|err| ClientError::Sidecar(err.to_string()))?;
            let content = serde_json::from_value(serde_json::json!({
                "type": "text",
                "text": prompt,
            }))
            .map_err(|err| ClientError::Sidecar(err.to_string()))?;
            let prompt_result = vm
                .prompt(PromptInput {
                    session_id: Some(session_id.clone()),
                    idempotency_key: None,
                    content: vec![content],
                })
                .await;
            // Always delete this per-run session so cron does not grow the durable catalog.
            let delete_result = vm.delete_session(Some(&session_id)).await;
            match (prompt_result, delete_result) {
                (Ok(_), Ok(())) => Ok(()),
                (Err(prompt_error), Ok(())) => Err(ClientError::Sidecar(prompt_error.to_string())),
                (Ok(_), Err(delete_error)) => Err(ClientError::Sidecar(format!(
                    "cron prompt completed but durable session cleanup failed: {delete_error}"
                ))),
                (Err(prompt_error), Err(delete_error)) => {
                    eprintln!(
                        "ERR_AGENTOS_CRON_SESSION_CLEANUP: prompt failed with {prompt_error}; durable session cleanup also failed: {delete_error}"
                    );
                    Err(ClientError::Sidecar(prompt_error.to_string()))
                }
            }
        }
        CronAction::Exec { command, args } => {
            // Send the structured argv verbatim. Flattening `command`/`args` into a single string
            // and re-parsing it through the `exec` command-line parser would re-split argv elements
            // on whitespace and shell-evaluate any `$()`/backtick content; `exec_argv` preserves the
            // structured (command, args) contract element-for-element.
            vm.exec_argv_process(command, args, crate::process::ExecOptions::default())
                .await
                .map_err(|err| ClientError::Sidecar(err.to_string()))?;
            Ok(())
        }
        CronAction::Callback { callback } => {
            callback().await;
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Schedule validation
// ---------------------------------------------------------------------------

/// A parsed schedule: either a recurring cron expression or a one-shot ISO-8601 timestamp.
///
/// Mirrors TS `ParsedSchedule` (`parse-schedule.ts`).
pub(crate) enum ParsedSchedule {
    /// A one-shot absolute timestamp.
    Date(DateTime<Utc>),
    /// A recurring cron expression (croner grammar).
    Cron(CronExpr),
}

impl ParsedSchedule {
    /// `true` for a recurring cron expression. Mirrors TS `parsed.kind === "cron"`.
    pub(crate) fn is_cron(&self) -> bool {
        matches!(self, ParsedSchedule::Cron(_))
    }
}

/// Resolve the next run for an already-parsed schedule strictly after `now`. Mirrors TS
/// `resolveSchedule(...).nextRun`: a cron yields `cron.nextRun()`; a one-shot date yields the date if
/// it is in the future, else `None`.
pub(crate) fn resolve_next_run(
    parsed: &ParsedSchedule,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match parsed {
        ParsedSchedule::Cron(cron) => cron.next_after(now),
        ParsedSchedule::Date(date) => {
            if date.timestamp_millis() > now.timestamp_millis() {
                Some(*date)
            } else {
                None
            }
        }
    }
}

/// Decide whether a schedule string looks like a one-shot ISO-8601-ish timestamp rather than a cron
/// expression. Mirrors TS `looksLikeOneShotSchedule` /
/// `^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d{1,3})?)?(?:Z|[+-]\d{2}:\d{2})?)?$`, with the
/// fractional-seconds group widened to accept any number of digits so a Rust-produced RFC-3339
/// timestamp (up to 9 fractional digits) is recognized as a one-shot.
fn looks_like_one_shot(schedule: &str) -> bool {
    let bytes = schedule.as_bytes();
    let mut i = 0usize;

    let is_digit = |b: u8| b.is_ascii_digit();

    let take_digits = |bytes: &[u8], i: &mut usize, n: usize| -> bool {
        for _ in 0..n {
            match bytes.get(*i) {
                Some(&b) if is_digit(b) => *i += 1,
                _ => return false,
            }
        }
        true
    };
    let take_lit = |bytes: &[u8], i: &mut usize, lit: u8| -> bool {
        match bytes.get(*i) {
            Some(&b) if b == lit => {
                *i += 1;
                true
            }
            _ => false,
        }
    };

    if !take_digits(bytes, &mut i, 4) {
        return false;
    }
    if !take_lit(bytes, &mut i, b'-') {
        return false;
    }
    if !take_digits(bytes, &mut i, 2) {
        return false;
    }
    if !take_lit(bytes, &mut i, b'-') {
        return false;
    }
    if !take_digits(bytes, &mut i, 2) {
        return false;
    }

    // Optional time portion: [T ]HH:MM(:SS(.fff)?)?(Z|[+-]HH:MM)?
    if i == bytes.len() {
        return true;
    }
    match bytes.get(i) {
        Some(b'T') | Some(b' ') => i += 1,
        _ => return false,
    }
    if !take_digits(bytes, &mut i, 2) {
        return false;
    }
    if !take_lit(bytes, &mut i, b':') {
        return false;
    }
    if !take_digits(bytes, &mut i, 2) {
        return false;
    }

    // Optional :SS
    if take_lit(bytes, &mut i, b':') {
        if !take_digits(bytes, &mut i, 2) {
            return false;
        }
        // Optional fractional seconds. The TS regex caps this at `\.\d{1,3}`, but a Rust-produced
        // one-shot from `chrono::DateTime::to_rfc3339()` emits up to 9 fractional digits, so a valid
        // near-future RFC-3339 timestamp must not be misclassified as a cron expression. Accept any
        // run of one or more fractional digits.
        if take_lit(bytes, &mut i, b'.') {
            let mut frac = 0;
            while matches!(bytes.get(i), Some(&b) if is_digit(b)) {
                i += 1;
                frac += 1;
            }
            if frac == 0 {
                return false;
            }
        }
    }

    // Optional timezone: Z | [+-]HH:MM
    match bytes.get(i) {
        None => return true,
        Some(b'Z') => {
            i += 1;
        }
        Some(b'+') | Some(b'-') => {
            i += 1;
            if !take_digits(bytes, &mut i, 2) {
                return false;
            }
            if !take_lit(bytes, &mut i, b':') {
                return false;
            }
            if !take_digits(bytes, &mut i, 2) {
                return false;
            }
        }
        _ => return false,
    }

    i == bytes.len()
}

/// Parse a one-shot timestamp string into a UTC instant, matching ECMAScript `Date.parse` rules for
/// the subset accepted by [`looks_like_one_shot`]:
/// - a date-only string (`2026-06-04`) is UTC midnight;
/// - a date-time string WITHOUT an offset (`2026-06-04T12:30`, `2026-06-04 12:30`) is parsed as LOCAL
///   time;
/// - forms with `Z` or an explicit numeric offset are parsed as written.
fn parse_one_shot(schedule: &str) -> Option<DateTime<Utc>> {
    use chrono::TimeZone;

    // Try a full RFC-3339 timestamp first (handles Z and numeric offsets).
    if let Ok(dt) = DateTime::parse_from_rfc3339(schedule) {
        return Some(dt.with_timezone(&Utc));
    }

    // Normalize a space separator to `T` for the naive parsers below.
    let normalized = schedule.replacen(' ', "T", 1);

    // Date + time without a timezone: ECMAScript treats this as LOCAL time.
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&normalized, fmt) {
            return match Local.from_local_datetime(&naive) {
                chrono::LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
                chrono::LocalResult::Ambiguous(dt, _) => Some(dt.with_timezone(&Utc)),
                chrono::LocalResult::None => None,
            };
        }
    }

    // Date only: midnight UTC (ECMAScript date-only form is UTC).
    if let Ok(date) = chrono::NaiveDate::parse_from_str(schedule, "%Y-%m-%d") {
        let naive = date.and_hms_opt(0, 0, 0)?;
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }

    None
}

/// Parse a schedule string into a [`ParsedSchedule`]. Mirrors TS `parseSchedule`.
pub(crate) fn parse_schedule(schedule: &str) -> std::result::Result<ParsedSchedule, ClientError> {
    let normalized = schedule.trim();
    if looks_like_one_shot(normalized) {
        return match parse_one_shot(normalized) {
            Some(date) => Ok(ParsedSchedule::Date(date)),
            None => Err(ClientError::InvalidSchedule(schedule.to_string())),
        };
    }

    match CronExpr::parse(normalized) {
        Ok(cron) => Ok(ParsedSchedule::Cron(cron)),
        Err(_) => Err(ClientError::InvalidSchedule(schedule.to_string())),
    }
}

/// Compute the next fire time for a schedule string strictly after `now`. Returns `None` for a
/// one-shot timestamp in the past or a cron expression with no upcoming match. Mirrors TS
/// `computeNextTime` / `resolveSchedule(...).nextRun`.
pub(crate) fn compute_next_time(schedule: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let parsed = parse_schedule(schedule).ok()?;
    resolve_next_run(&parsed, now)
}

/// Validate a schedule string. Returns the parsed next run for one-shot ISO-8601 schedules.
///
/// Errors `InvalidSchedule` for malformed input and `PastSchedule` for one-shot timestamps already
/// in the past. Mirrors TS `validateScheduleForRegistration`: a one-shot timestamp that resolves to
/// no next run is rejected as `PastSchedule`; cron expressions are accepted even when their next run
/// is currently unknown.
pub(crate) fn validate_schedule(
    schedule: &str,
    now: DateTime<Utc>,
) -> std::result::Result<Option<DateTime<Utc>>, ClientError> {
    let parsed = parse_schedule(schedule)?;
    match parsed {
        ParsedSchedule::Cron(cron) => Ok(cron.next_after(now)),
        ParsedSchedule::Date(date) => {
            if date.timestamp_millis() > now.timestamp_millis() {
                Ok(Some(date))
            } else {
                Err(ClientError::PastSchedule(schedule.to_string()))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Cron expression parser + next-run search (croner-compatible grammar)
// ---------------------------------------------------------------------------

/// A parsed cron expression interpreted in the host LOCAL timezone (matching croner's default).
///
/// Implemented in-crate because the workspace has no cron-parsing dependency. Accepts the croner
/// grammar: 5-field (`min hour dom month dow`), 6-field (leading `seconds`), and 7-field (leading
/// `seconds`, trailing `year`) expressions; named months (`JAN`-`DEC`); named weekdays (`SUN`-`SAT`);
/// `*`, ranges (`a-b`), steps (`*/n`, `a-b/n`, `a/n`), comma lists, `?` (treated as `*` for
/// dom/dow), `L` (last day of month for dom, last weekday-of-month for dow), `#` (nth weekday), and
/// `W` (nearest weekday to a day-of-month). Day-of-month and day-of-week combine with OR semantics
/// when both are restricted, matching Vixie/croner.
pub(crate) struct CronExpr {
    seconds: Vec<u32>,
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days_of_month: Vec<u32>,
    months: Vec<u32>,
    days_of_week: Vec<u32>,
    years: Option<Vec<u32>>,
    dom_restricted: bool,
    dow_restricted: bool,
    /// Day-of-month `L` (last day of month).
    dom_last: bool,
    /// Day-of-month `LW` (last weekday, Mon-Fri, on or before the last day of the month).
    dom_last_weekday: bool,
    /// Day-of-month `<n>W` (nearest weekday to day `n`).
    dom_nearest_weekday: Option<u32>,
    /// Day-of-week `<weekday>L` (last given weekday of the month).
    dow_last: Option<u32>,
    /// Day-of-week `<weekday>#<n>` (nth given weekday of the month).
    dow_nth: Option<(u32, u32)>,
}

const MONTH_NAMES: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAY_NAMES: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

impl CronExpr {
    fn parse(expr: &str) -> std::result::Result<Self, ()> {
        let fields: Vec<&str> = expr.split_whitespace().collect();

        // Accept 5, 6, or 7 fields. 6-field adds a leading seconds field; 7-field adds a trailing
        // year field on top of that. Mirrors croner's field-count handling.
        let (sec, min, hour, dom, month, dow, year): (
            &str,
            &str,
            &str,
            &str,
            &str,
            &str,
            Option<&str>,
        ) = match fields.len() {
            5 => (
                "0", fields[0], fields[1], fields[2], fields[3], fields[4], None,
            ),
            6 => (
                fields[0], fields[1], fields[2], fields[3], fields[4], fields[5], None,
            ),
            7 => (
                fields[0],
                fields[1],
                fields[2],
                fields[3],
                fields[4],
                fields[5],
                Some(fields[6]),
            ),
            _ => return Err(()),
        };

        let seconds = parse_field(sec, 0, 59, FieldKind::Plain)?;
        let minutes = parse_field(min, 0, 59, FieldKind::Plain)?;
        let hours = parse_field(hour, 0, 23, FieldKind::Plain)?;

        let mut dom_last = false;
        let mut dom_last_weekday = false;
        let mut dom_nearest_weekday = None;
        let days_of_month = parse_dom_field(
            dom,
            &mut dom_last,
            &mut dom_last_weekday,
            &mut dom_nearest_weekday,
        )?;

        let months = parse_field(month, 1, 12, FieldKind::Month)?;

        let mut dow_last = None;
        let mut dow_nth = None;
        let days_of_week = parse_dow_field(dow, &mut dow_last, &mut dow_nth)?;

        let years = match year {
            Some(y) => Some(parse_field(y, 1970, 2099, FieldKind::Plain)?),
            None => None,
        };

        // `?` is equivalent to `*` for matching purposes, so the field is "unrestricted".
        let dom_restricted = dom != "*" && dom != "?";
        let dow_restricted = dow != "*" && dow != "?";

        Ok(Self {
            seconds,
            minutes,
            hours,
            days_of_month,
            months,
            days_of_week,
            years,
            dom_restricted,
            dow_restricted,
            dom_last,
            dom_last_weekday,
            dom_nearest_weekday,
            dow_last,
            dow_nth,
        })
    }

    /// Find the next instant strictly after `after` (truncated to whole seconds) that matches, in the
    /// LOCAL timezone. Scans second-by-second only when a sub-minute (seconds) constraint is present;
    /// otherwise scans minute-by-minute. Bounded so an impossible expression terminates.
    fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let local_after = after.with_timezone(&Local);

        // Determine the step granularity. When seconds is the default `[0]` we can step by minutes.
        let by_seconds = self.seconds != vec![0];

        let step = if by_seconds {
            ChronoDuration::seconds(1)
        } else {
            ChronoDuration::minutes(1)
        };

        let mut candidate = if by_seconds {
            local_after.with_nanosecond(0)? + ChronoDuration::seconds(1)
        } else {
            local_after.with_second(0)?.with_nanosecond(0)? + ChronoDuration::minutes(1)
        };

        // Bound the search: a few years of ticks so an impossible expression terminates.
        let max_iterations: u64 = if by_seconds {
            // ~2 years of seconds.
            2u64 * 366 * 24 * 60 * 60
        } else {
            // ~6 years of minutes (years field can push matches far out).
            6u64 * 366 * 24 * 60
        };
        for _ in 0..max_iterations {
            if self.matches_local(&candidate) {
                return Some(candidate.with_timezone(&Utc));
            }
            candidate += step;
        }
        None
    }

    fn matches_local(&self, dt: &DateTime<Local>) -> bool {
        if !self.seconds.contains(&dt.second()) {
            return false;
        }
        if !self.minutes.contains(&dt.minute()) {
            return false;
        }
        if !self.hours.contains(&dt.hour()) {
            return false;
        }
        if !self.months.contains(&dt.month()) {
            return false;
        }
        if let Some(years) = &self.years {
            let year = dt.year();
            if year < 0 || !years.contains(&(year as u32)) {
                return false;
            }
        }

        let dom_match = self.dom_matches(dt);
        let dow_match = self.dow_matches(dt);

        // Vixie/croner OR semantics: if both DOM and DOW are restricted, a match in either suffices;
        // if only one is restricted, only that one is consulted; if neither, both pass.
        match (self.dom_restricted, self.dow_restricted) {
            (true, true) => dom_match || dow_match,
            (true, false) => dom_match,
            (false, true) => dow_match,
            (false, false) => true,
        }
    }

    fn dom_matches(&self, dt: &DateTime<Local>) -> bool {
        let dom = dt.day();
        if self.dom_last && dom == last_day_of_month(dt.year(), dt.month()) {
            return true;
        }
        if self.dom_last_weekday {
            // Last weekday (Mon-Fri) on or before the last day of the month: the nearest-weekday
            // resolution of the last day handles the Saturday/Sunday shift back into the month.
            if is_nearest_weekday(dt, last_day_of_month(dt.year(), dt.month())) {
                return true;
            }
        }
        if let Some(target) = self.dom_nearest_weekday {
            if is_nearest_weekday(dt, target) {
                return true;
            }
        }
        self.days_of_month.contains(&dom)
    }

    fn dow_matches(&self, dt: &DateTime<Local>) -> bool {
        let dow = weekday_sun0(dt.weekday());

        if let Some(target) = self.dow_last {
            // Last occurrence of `target` weekday in this month.
            if dow == target {
                let next_week = *dt + ChronoDuration::days(7);
                if next_week.month() != dt.month() {
                    return true;
                }
            }
        }
        if let Some((target, n)) = self.dow_nth {
            if dow == target {
                // 1-based occurrence index of this weekday within the month.
                let occurrence = (dt.day() - 1) / 7 + 1;
                if occurrence == n {
                    return true;
                }
            }
        }
        self.days_of_week.contains(&dow)
    }
}

/// Convert chrono `Weekday` to cron's `Sun=0..Sat=6` numbering.
fn weekday_sun0(weekday: Weekday) -> u32 {
    weekday.num_days_from_sunday()
}

/// Last calendar day of a given month.
fn last_day_of_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_next = chrono::NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid first-of-month");
    (first_next - ChronoDuration::days(1)).day()
}

/// Whether `dt` is the nearest weekday (Mon-Fri) to day-of-month `target` within the same month,
/// per cron `W` semantics. If `target` falls on a weekend, the nearest weekday in the same month is
/// used (Saturday shifts to Friday, Sunday shifts to Monday); a shift never crosses the month
/// boundary.
fn is_nearest_weekday(dt: &DateTime<Local>, target: u32) -> bool {
    let last = last_day_of_month(dt.year(), dt.month());
    let target = target.min(last);
    let target_date = chrono::NaiveDate::from_ymd_opt(dt.year(), dt.month(), target);
    let target_date = match target_date {
        Some(d) => d,
        None => return false,
    };
    let target_weekday = target_date.weekday();
    let resolved_day = match target_weekday {
        Weekday::Sat => {
            if target > 1 {
                target - 1
            } else {
                // Saturday on the 1st shifts forward to Monday (the 3rd).
                target + 2
            }
        }
        Weekday::Sun => {
            if target < last {
                target + 1
            } else {
                // Sunday on the last day shifts back to Friday.
                target - 2
            }
        }
        Weekday::Mon | Weekday::Tue | Weekday::Wed | Weekday::Thu | Weekday::Fri => target,
    };
    dt.day() == resolved_day
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    Plain,
    Month,
    Weekday,
}

/// Parse a numeric/named cron field (`*`, `?`, lists, ranges, steps) into the sorted set of matching
/// values within `[min, max]`. `?` is treated as `*`. For [`FieldKind::Month`] names `JAN`-`DEC` are
/// accepted.
fn parse_field(
    field: &str,
    min: u32,
    max: u32,
    kind: FieldKind,
) -> std::result::Result<Vec<u32>, ()> {
    if field == "?" {
        // `?` = no specific value; treat as the full range.
        return Ok((min..=max).collect());
    }
    let mut values: Vec<u32> = Vec::new();
    for part in field.split(',') {
        if part.is_empty() {
            return Err(());
        }
        parse_field_part(part, min, max, kind, &mut values)?;
    }
    if values.is_empty() {
        return Err(());
    }
    values.sort_unstable();
    values.dedup();
    Ok(values)
}

/// Parse the day-of-month field, recognizing `L` (last day), `LW` (last weekday), and `<n>W` (nearest
/// weekday to day `n`) in addition to the standard grammar. Mirrors croner: `W` must be preceded by
/// `L` or a single day-of-month value in `1..=31` (`W` alone, `0W`, and `32W` are rejected).
fn parse_dom_field(
    field: &str,
    dom_last: &mut bool,
    dom_last_weekday: &mut bool,
    dom_nearest_weekday: &mut Option<u32>,
) -> std::result::Result<Vec<u32>, ()> {
    let upper = field.to_ascii_uppercase();
    if upper == "L" {
        *dom_last = true;
        // No fixed numeric days; matching handled by `dom_last`.
        return Ok(Vec::new());
    }
    if upper == "LW" {
        *dom_last_weekday = true;
        return Ok(Vec::new());
    }
    if let Some(stripped) = upper.strip_suffix('W') {
        let day: u32 = stripped.parse().map_err(|_| ())?;
        if !(1..=31).contains(&day) {
            return Err(());
        }
        *dom_nearest_weekday = Some(day);
        return Ok(Vec::new());
    }
    parse_field(field, 1, 31, FieldKind::Plain)
}

/// Parse the day-of-week field, recognizing `<weekday>L` (last weekday-of-month) and
/// `<weekday>#<n>` (nth weekday-of-month), named weekdays, and `7` folded onto Sunday.
fn parse_dow_field(
    field: &str,
    dow_last: &mut Option<u32>,
    dow_nth: &mut Option<(u32, u32)>,
) -> std::result::Result<Vec<u32>, ()> {
    let upper = field.to_ascii_uppercase();

    // `<weekday>#<n>` (nth weekday of the month).
    if let Some((wd, nth)) = upper.split_once('#') {
        let weekday = parse_weekday_token(wd)?;
        let n: u32 = nth.parse().map_err(|_| ())?;
        if !(1..=5).contains(&n) {
            return Err(());
        }
        *dow_nth = Some((weekday, n));
        return Ok(Vec::new());
    }

    // `<weekday>L` (last given weekday of the month).
    if let Some(stripped) = upper.strip_suffix('L') {
        let weekday = parse_weekday_token(stripped)?;
        *dow_last = Some(weekday);
        return Ok(Vec::new());
    }

    if upper == "?" || upper == "*" {
        let mut v = parse_field(field, 0, 7, FieldKind::Plain)?;
        fold_sunday(&mut v);
        return Ok(v);
    }

    let mut values = parse_field(field, 0, 7, FieldKind::Weekday)?;
    fold_sunday(&mut values);
    Ok(values)
}

/// Fold `7` (Sunday) onto `0` and dedupe.
fn fold_sunday(values: &mut Vec<u32>) {
    for v in values.iter_mut() {
        if *v == 7 {
            *v = 0;
        }
    }
    values.sort_unstable();
    values.dedup();
}

/// Parse a single weekday token (numeric `0`-`7` or named `SUN`-`SAT`) to `Sun=0..Sat=6`.
fn parse_weekday_token(token: &str) -> std::result::Result<u32, ()> {
    let upper = token.to_ascii_uppercase();
    if let Some(idx) = WEEKDAY_NAMES.iter().position(|name| *name == upper) {
        return Ok(idx as u32);
    }
    let v: u32 = upper.parse().map_err(|_| ())?;
    match v {
        0..=6 => Ok(v),
        7 => Ok(0),
        _ => Err(()),
    }
}

// Re-add FieldKind::Weekday support by extending parse_field via a wrapper for weekday names.
impl FieldKind {
    fn resolve_name(self, token: &str) -> Option<u32> {
        let upper = token.to_ascii_uppercase();
        match self {
            FieldKind::Plain => None,
            FieldKind::Month => MONTH_NAMES
                .iter()
                .position(|name| *name == upper)
                .map(|i| (i + 1) as u32),
            FieldKind::Weekday => WEEKDAY_NAMES
                .iter()
                .position(|name| *name == upper)
                .map(|i| i as u32),
        }
    }
}

fn parse_field_part(
    part: &str,
    min: u32,
    max: u32,
    kind: FieldKind,
    out: &mut Vec<u32>,
) -> std::result::Result<(), ()> {
    // Split off an optional step (`.../n`).
    let (range_spec, step) = match part.split_once('/') {
        Some((range_spec, step_str)) => {
            let step: u32 = step_str.parse().map_err(|_| ())?;
            if step == 0 {
                return Err(());
            }
            (range_spec, Some(step))
        }
        None => (part, None),
    };

    // Determine the [start, end] bounds for this part.
    let (start, end) = if range_spec == "*" {
        (min, max)
    } else if let Some((lo, hi)) = range_spec.split_once('-') {
        let lo = parse_value_token(lo, kind)?;
        let hi = parse_value_token(hi, kind)?;
        (lo, hi)
    } else {
        // A bare numeric value may not carry a step. croner rejects `5/15` / `0/5`
        // ("stepping with numeric prefix"); only `*` or an explicit range may precede `/`.
        if step.is_some() {
            return Err(());
        }
        let v = parse_value_token(range_spec, kind)?;
        (v, v)
    };

    if start < min || end > max || start > end {
        return Err(());
    }

    let step = step.unwrap_or(1);
    let mut v = start;
    while v <= end {
        out.push(v);
        v += step;
    }
    Ok(())
}

/// Parse a single value token: a number, or a name (month names for [`FieldKind::Month`], weekday
/// names for [`FieldKind::Weekday`]).
fn parse_value_token(token: &str, kind: FieldKind) -> std::result::Result<u32, ()> {
    match kind {
        FieldKind::Weekday => parse_weekday_token(token),
        FieldKind::Month => {
            if let Some(v) = kind.resolve_name(token) {
                return Ok(v);
            }
            token.parse().map_err(|_| ())
        }
        FieldKind::Plain => token.parse().map_err(|_| ()),
    }
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

impl AgentOs {
    /// Schedule a cron job. SYNC. Validates the schedule (errors `InvalidSchedule` / `PastSchedule`).
    /// `id` defaults to a UUID; `overlap` defaults to allow.
    ///
    /// Mirrors TS `AgentOs.scheduleCron` / `CronManager.schedule`: validation happens up front, the
    /// driver is asked to arm the timer (`this.driver.schedule({ id, schedule, callback })`), and the
    /// job is registered. The driver owns all timing: it parses the schedule, fires the callback,
    /// reschedules cron after each fire, and is cancelled on [`CronJobHandle::cancel`] /
    /// [`CronManager::dispose`]. The returned [`CronJobHandle`] cancels the job.
    pub fn schedule_cron(
        &self,
        options: CronJobOptions,
    ) -> std::result::Result<CronJobHandle, ClientError> {
        let cron = self.cron();
        let now = Utc::now();

        // Validate before any state mutation, matching TS `validateScheduleForRegistration`.
        let next_run = validate_schedule(&options.schedule, now)?;

        let id = options
            .id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let overlap = options.overlap.unwrap_or_default();

        // Build the driver callback that runs one job execution, mirroring TS
        // `callback: () => this.executeJob(id)`.
        let manager = Arc::clone(cron);
        let vm = self.clone();
        let callback_id = id.clone();
        let callback: crate::config::ScheduleCallback = Arc::new(move || {
            let manager = Arc::clone(&manager);
            let vm = vm.clone();
            let id = callback_id.clone();
            Box::pin(async move {
                execute_job(manager, vm, id).await;
            })
        });

        register_cron_job(
            cron,
            id,
            options.schedule,
            options.action,
            overlap,
            next_run,
            callback,
        )
    }

    /// Snapshot all cron jobs. Mirrors TS `CronManager.list`.
    pub fn list_cron_jobs(&self) -> Vec<CronJobInfo> {
        let mut result = Vec::new();
        self.cron().jobs.scan(|id, state| {
            result.push(CronJobInfo {
                id: id.clone(),
                schedule: state.schedule.clone(),
                action: CronActionInfo::from(&state.action),
                overlap: state.overlap,
                last_run: state
                    .last_run
                    .lock()
                    .as_ref()
                    .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true)),
                next_run: state
                    .next_run
                    .lock()
                    .as_ref()
                    .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true)),
                run_count: state.run_count.load(Ordering::SeqCst),
                running: state.running.load(Ordering::SeqCst),
            });
        });
        result
    }

    /// Cancel a cron job. No-op if unknown; never errors. Mirrors TS `CronManager.cancel`.
    pub fn cancel_cron_job(&self, id: &str) {
        self.cron().cancel_job(id);
    }

    /// Subscribe to cron events. The TS API returns no unsubscribe; dropping the receiver is the
    /// equivalent. Each run emits `Fire` then `Complete`|`Error`. Mirrors TS `AgentOs.onCronEvent`.
    pub fn cron_events(&self) -> broadcast::Receiver<CronEvent> {
        self.cron().event_tx.subscribe()
    }
}

fn ensure_cron_capacity(cron: &CronManager, id: &str) -> std::result::Result<(), ClientError> {
    if cron.jobs.contains(id) || cron.jobs.len() < crate::CRON_JOB_LIMIT {
        return Ok(());
    }

    Err(ClientError::Sidecar(format!(
        "cron job limit exceeded: at most {} jobs can be scheduled per VM",
        crate::CRON_JOB_LIMIT
    )))
}

fn register_cron_job(
    cron: &Arc<CronManager>,
    id: String,
    schedule: String,
    action: CronAction,
    overlap: CronOverlap,
    next_run: Option<DateTime<Utc>>,
    callback: crate::config::ScheduleCallback,
) -> std::result::Result<CronJobHandle, ClientError> {
    let _guard = cron.schedule_lock.lock();
    ensure_cron_capacity(cron, &id)?;

    // If replacing an existing id, cancel the old driver-armed timer before scheduling the new one.
    // The default timer driver's handles are id-based, so cancelling after the new schedule would
    // cancel the replacement.
    if let Some((_, old)) = cron.jobs.remove(&id) {
        cron.driver.cancel(&old.handle);
    }

    let handle = cron.driver.schedule(ScheduleEntry {
        id: id.clone(),
        schedule: schedule.clone(),
        callback,
    });

    let state = CronJobState {
        schedule,
        action,
        overlap,
        last_run: parking_lot::Mutex::new(None),
        next_run: parking_lot::Mutex::new(next_run),
        run_count: std::sync::atomic::AtomicU64::new(0),
        running: AtomicBool::new(false),
        queued: AtomicBool::new(false),
        handle,
    };

    let _ = cron.jobs.insert(id.clone(), state);

    Ok(CronJobHandle {
        id,
        manager: Arc::clone(cron),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ensure_cron_capacity, register_cron_job, CronAction, CronJobState, CronManager,
        CronOverlap, ScheduleDriver, ScheduleEntry, ScheduleHandle,
    };
    use crate::CRON_JOB_LIMIT;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[derive(Default)]
    struct RecordingScheduleDriver {
        calls: parking_lot::Mutex<Vec<String>>,
    }

    impl ScheduleDriver for RecordingScheduleDriver {
        fn schedule(&self, entry: ScheduleEntry) -> ScheduleHandle {
            self.calls.lock().push(format!("schedule:{}", entry.id));
            ScheduleHandle { id: entry.id }
        }

        fn cancel(&self, handle: &ScheduleHandle) {
            self.calls.lock().push(format!("cancel:{}", handle.id));
        }

        fn dispose(&self) {}
    }

    fn dummy_state(id: String) -> CronJobState {
        CronJobState {
            schedule: "0 0 * * *".to_string(),
            action: CronAction::Callback {
                callback: Arc::new(|| Box::pin(async {})),
            },
            overlap: CronOverlap::Allow,
            last_run: parking_lot::Mutex::new(None),
            next_run: parking_lot::Mutex::new(None),
            run_count: std::sync::atomic::AtomicU64::new(0),
            running: AtomicBool::new(false),
            queued: AtomicBool::new(false),
            handle: ScheduleHandle { id },
        }
    }

    #[test]
    fn cron_capacity_rejects_new_jobs_at_limit_but_allows_replacements() {
        let manager = CronManager::new(Arc::new(RecordingScheduleDriver::default()));
        for index in 0..CRON_JOB_LIMIT {
            let id = format!("job-{index}");
            assert!(
                manager.jobs.insert(id.clone(), dummy_state(id)).is_ok(),
                "seed cron job"
            );
        }

        let error = ensure_cron_capacity(&manager, "overflow").expect_err("limit should reject");
        assert!(
            error.to_string().contains("cron job limit exceeded"),
            "unexpected limit error: {error}"
        );
        ensure_cron_capacity(&manager, "job-0").expect("replacement should be allowed");
    }

    // ── Security: AOSCLIENT-P1-cron-exec (N-007 untrusted cron CronAction::Exec) ─────────────────
    //
    // Threat: an untrusted actor schedules a `CronAction::Exec { command, args }` whose `args`
    // carry data values (a path with spaces) or shell metacharacters (`$( )`, backticks). The
    // intent of a structured `(command, args)` action is that the args are passed VERBATIM as
    // argv elements — never re-split on whitespace, never re-evaluated by a shell.
    //
    // The bug (now fixed): `run_action`'s `CronAction::Exec` arm flattened the pair with
    // `format!("{} {}", command, args.join(" "))` and handed the STRING to `AgentOs::exec`, which
    // re-parsed it through `resolve_exec_command`. That round-trip (a) re-split `"a b"` into two
    // argv elements and (b)/(c) promoted `$(id)` / backtick elements to a real `sh -c` shell
    // evaluation. The fix sends the structured argv verbatim via `AgentOs::exec_argv`, bypassing
    // `resolve_exec_command` entirely.
    //
    // This test pins the fix: it computes the argv exactly as the fixed `CronAction::Exec` arm
    // does (verbatim `command` + `args`), and asserts the hostile elements survive intact. As a
    // negative control it also shows the OLD join+`resolve_exec_command` path corrupts them, so a
    // regression back to the flatten behavior fails this test.
    #[test]
    fn cron_exec_action_argv_is_not_shell_re_split_or_evaluated() {
        // The fixed `CronAction::Exec` arm passes `command` and `args` straight to `exec_argv`,
        // which sends them verbatim with no `resolve_exec_command` round-trip.
        fn cron_exec_argv(command: &str, args: &[&str]) -> (String, Vec<String>) {
            (
                command.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            )
        }

        // The pre-fix flatten+re-parse path, kept here purely as a negative control.
        fn buggy_join_then_resolve(command: &str, args: &[&str]) -> (String, Vec<String>) {
            let joined = if args.is_empty() {
                command.to_string()
            } else {
                format!("{} {}", command, args.join(" "))
            };
            crate::command_line::resolve_exec_command(&joined).expect("line must resolve")
        }

        // (a) A single argv element that contains a space MUST stay one argv element.
        let (cmd, args) = cron_exec_argv("printenv", &["a b"]);
        assert_eq!(
            (cmd.as_str(), args.as_slice()),
            ("printenv", &["a b".to_string()][..]),
            "N-007: structured argv element \"a b\" must survive as a single argv element"
        );
        // Negative control: the old path corrupts it by re-splitting on whitespace.
        let (_, buggy_args) = buggy_join_then_resolve("printenv", &["a b"]);
        assert_eq!(
            buggy_args,
            vec!["a".to_string(), "b".to_string()],
            "N-007 negative control: the old join+resolve path re-split \"a b\" into two argv elements"
        );

        // (b) A command-substitution argv element MUST stay a literal argv element, never `sh -c`.
        let (cmd, args) = cron_exec_argv("printenv", &["$(id)"]);
        assert_eq!(
            (cmd.as_str(), args.as_slice()),
            ("printenv", &["$(id)".to_string()][..]),
            "N-007: command-substitution argv element \"$(id)\" must NOT be promoted to `sh -c`"
        );
        // Negative control: the old path routes the whole line through `sh -c`, evaluating `$(id)`.
        let (buggy_cmd, _) = buggy_join_then_resolve("printenv", &["$(id)"]);
        assert_eq!(
            buggy_cmd, "sh",
            "N-007 negative control: the old path promoted the `$(id)` line to a `sh -c` shell"
        );

        // (c) A backtick argv element: same guarantee.
        let (cmd, args) = cron_exec_argv("printenv", &["`id`"]);
        assert_eq!(
            (cmd.as_str(), args.as_slice()),
            ("printenv", &["`id`".to_string()][..]),
            "N-007: backtick argv element \"`id`\" must NOT be promoted to `sh -c`"
        );
        let (buggy_cmd, _) = buggy_join_then_resolve("printenv", &["`id`"]);
        assert_eq!(
            buggy_cmd, "sh",
            "N-007 negative control: the old path promoted the backtick line to a `sh -c` shell"
        );
    }

    // ── Security: AOSCLIENT-P2-cron-cap (N-008 cron job-limit flooding) ──────────────────────────
    //
    // Threat: an untrusted actor floods the cron registry to exhaust host scheduling resources.
    // The public `AgentOs::schedule_cron` registers through `register_cron_job` ->
    // `ensure_cron_capacity` (cron.rs:1162), which must cap distinct jobs at `CRON_JOB_LIMIT`
    // while still allowing an existing id to be REPLACED at the cap. `AgentOs::schedule_cron`
    // itself needs a live sidecar to construct, so we drive the exact same public registration
    // chokepoint (`register_cron_job`) the public method funnels into, with a recording driver.
    #[test]
    fn schedule_cron_public_path_rejects_jobs_beyond_cron_job_limit() {
        let driver = Arc::new(RecordingScheduleDriver::default());
        let manager = Arc::new(CronManager::new(driver.clone()));

        let make_callback =
            || -> crate::config::ScheduleCallback { Arc::new(|| Box::pin(async {})) };

        // Fill the registry to exactly CRON_JOB_LIMIT distinct ids through the public chokepoint.
        for index in 0..CRON_JOB_LIMIT {
            register_cron_job(
                &manager,
                format!("flood-{index}"),
                "0 0 * * *".to_string(),
                CronAction::Callback {
                    callback: make_callback(),
                },
                CronOverlap::Allow,
                None,
                make_callback(),
            )
            .unwrap_or_else(|err| panic!("seed job {index} should register: {err}"));
        }
        assert_eq!(manager.jobs.len(), CRON_JOB_LIMIT);

        // The CRON_JOB_LIMIT+1-th DISTINCT id must be denied. (Match instead of `.expect_err()`
        // because the Ok type `CronJobHandle` does not implement Debug.)
        let overflow = match register_cron_job(
            &manager,
            "flood-overflow".to_string(),
            "0 0 * * *".to_string(),
            CronAction::Callback {
                callback: make_callback(),
            },
            CronOverlap::Allow,
            None,
            make_callback(),
        ) {
            Ok(_) => {
                panic!("AOSCLIENT-P2-cron-cap: the job beyond CRON_JOB_LIMIT must be rejected")
            }
            Err(err) => err,
        };
        assert!(
            overflow.to_string().contains("cron job limit exceeded"),
            "AOSCLIENT-P2-cron-cap: overflow rejection must report the cron job limit, got: {overflow}"
        );
        assert_eq!(
            manager.jobs.len(),
            CRON_JOB_LIMIT,
            "AOSCLIENT-P2-cron-cap: a rejected overflow job must not be inserted"
        );

        // Replacing an EXISTING id while at the cap must still succeed (replace, not grow).
        register_cron_job(
            &manager,
            "flood-0".to_string(),
            "0 1 * * *".to_string(),
            CronAction::Callback {
                callback: make_callback(),
            },
            CronOverlap::Allow,
            None,
            make_callback(),
        )
        .expect("AOSCLIENT-P2-cron-cap: replacing an existing id at the cap must be allowed");
        assert_eq!(
            manager.jobs.len(),
            CRON_JOB_LIMIT,
            "AOSCLIENT-P2-cron-cap: replacing an existing id must not grow the registry past the cap"
        );
    }

    #[test]
    fn cron_replacement_cancels_old_timer_before_scheduling_new_timer() {
        let driver = Arc::new(RecordingScheduleDriver::default());
        let manager = Arc::new(CronManager::new(driver.clone()));
        let callback: crate::config::ScheduleCallback = Arc::new(|| Box::pin(async {}));

        register_cron_job(
            &manager,
            "same-id".to_string(),
            "0 0 * * *".to_string(),
            CronAction::Callback {
                callback: callback.clone(),
            },
            CronOverlap::Allow,
            None,
            callback.clone(),
        )
        .expect("initial schedule");
        register_cron_job(
            &manager,
            "same-id".to_string(),
            "0 1 * * *".to_string(),
            CronAction::Callback { callback },
            CronOverlap::Allow,
            None,
            Arc::new(|| Box::pin(async {})),
        )
        .expect("replacement schedule");

        assert_eq!(
            *driver.calls.lock(),
            vec!["schedule:same-id", "cancel:same-id", "schedule:same-id"]
        );
        assert_eq!(manager.jobs.len(), 1);
    }
}
