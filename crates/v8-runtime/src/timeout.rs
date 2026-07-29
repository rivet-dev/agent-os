// Execution budget enforcement via process-owned runtime tasks.
//
// Two INDEPENDENT mechanisms live here:
//
//   * `TimeoutGuard` — a WALL-CLOCK timer. It counts elapsed real time
//     INCLUDING idle/await, so it can cap a guest that blocks or awaits
//     indefinitely. It is an INDEPENDENT, opt-in backstop armed only when the
//     operator sets `limits.jsRuntime.wallClockLimitMs` (off by default so
//     long-lived ACP adapters are never killed by a default).
//
//   * `CpuBudgetGuard` — a TRUE CPU-TIME budget. It samples the EXECUTION
//     thread's per-thread CPU clock (`pthread_getcpuclockid` +
//     `clock_gettime`). Because a thread's CPU clock does not advance while the
//     thread is parked/awaiting I/O, this counts ONLY active JS CPU time and
//     EXCLUDES idle/await. V8 has no native budget primitive, so this poll +
//     `terminate_execution()` approach is the standard embedder pattern. Armed
//     when the caller passes a nonzero `limits.jsRuntime.cpuTimeLimitMs`.
//     agentos sidecar VM executions supply a bounded default; lower-level
//     embedders may pass `None`/`0` to leave the guard disabled.
//
// The two guards are independent: setting one typed limit arms only that guard,
// and when both are set whichever fires first terminates execution.

use agentos_bridge::queue_tracker::{register_limit, TrackedLimit};
use agentos_runtime::{RuntimeContext, TaskClass, TaskOwner};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const TIMEOUT_GUARD_START_ERROR_CODE: &str = "ERR_TIMEOUT_GUARD_START";
#[cfg_attr(test, allow(dead_code))]
pub(crate) const CPU_BUDGET_GUARD_START_ERROR_CODE: &str = "ERR_CPU_BUDGET_GUARD_START";

/// How often the CPU-budget watchdog samples the execution thread's CPU clock.
#[cfg_attr(test, allow(dead_code))]
const CPU_BUDGET_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// An opaque handle to a specific thread's CPU-time clock, captured ON that
/// thread and safe to read from another (watchdog) thread.
///
/// The POSIX per-thread CPU clock id is derived from the thread's `pthread_t`
/// and remains valid for the lifetime of that thread, so the watchdog can poll
/// it via `clock_gettime` without running on the execution thread itself.
///
/// macOS has no `pthread_getcpuclockid`/per-thread POSIX clock, so it uses a
/// separate Mach-based implementation below (`pthread_mach_thread_np` +
/// `thread_info`) that exposes the same opaque `ThreadCpuClock` interface.
#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(test, allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) struct ThreadCpuClock {
    clockid: libc::clockid_t,
}

/// Capture the CALLING thread's CPU-time clock. Must be invoked on the thread
/// whose CPU time should be measured (i.e. the execution thread).
///
/// Returns `None` if the platform refuses to expose a per-thread CPU clock, in
/// which case no CPU budget can be enforced.
#[cfg(all(unix, not(target_os = "macos")))]
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn current_thread_cpu_clock() -> Option<ThreadCpuClock> {
    // SAFETY: `pthread_self` is always callable; `pthread_getcpuclockid` writes
    // a valid clockid into `clockid` on success (return 0).
    unsafe {
        let mut clockid: libc::clockid_t = 0;
        let rc = libc::pthread_getcpuclockid(libc::pthread_self(), &mut clockid);
        if rc == 0 {
            Some(ThreadCpuClock { clockid })
        } else {
            None
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl ThreadCpuClock {
    /// Read accumulated CPU time for the captured thread, in milliseconds.
    /// Returns `None` if the clock read fails.
    #[cfg_attr(test, allow(dead_code))]
    fn elapsed_ms(self) -> Option<u64> {
        // SAFETY: `clockid` came from a successful `pthread_getcpuclockid`; the
        // timespec is fully written by `clock_gettime` on success.
        unsafe {
            let mut ts: libc::timespec = std::mem::zeroed();
            if libc::clock_gettime(self.clockid, &mut ts) == 0 {
                let ms = (ts.tv_sec as i128) * 1_000 + (ts.tv_nsec as i128) / 1_000_000;
                Some(ms.max(0) as u64)
            } else {
                None
            }
        }
    }
}

/// macOS per-thread CPU clock. There is no `pthread_getcpuclockid` on Apple
/// platforms, so the thread's CPU time is read through the Mach
/// `thread_info(THREAD_BASIC_INFO)` call. The Mach thread port obtained via
/// `pthread_mach_thread_np` stays valid for the thread's lifetime and may be
/// inspected from another (watchdog) thread, matching the opaque-handle
/// contract above.
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
#[derive(Clone, Copy)]
pub(crate) struct ThreadCpuClock {
    port: libc::mach_port_t,
}

#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
pub(crate) fn current_thread_cpu_clock() -> Option<ThreadCpuClock> {
    // SAFETY: `pthread_mach_thread_np` returns the Mach thread port for the
    // calling pthread; the port is valid for the thread's lifetime.
    let port = unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) };
    // MACH_PORT_NULL is 0.
    if port == 0 {
        None
    } else {
        Some(ThreadCpuClock { port })
    }
}

#[cfg(target_os = "macos")]
impl ThreadCpuClock {
    /// Read accumulated CPU time (user + system) for the captured thread, in
    /// milliseconds. Returns `None` if the Mach query fails.
    #[cfg_attr(test, allow(dead_code))]
    fn elapsed_ms(self) -> Option<u64> {
        // SAFETY: `thread_info` fully initialises `info` when it returns
        // KERN_SUCCESS; the count is the documented THREAD_BASIC_INFO length.
        unsafe {
            let mut info = std::mem::MaybeUninit::<libc::thread_basic_info>::zeroed();
            let mut count = (std::mem::size_of::<libc::thread_basic_info>()
                / std::mem::size_of::<libc::integer_t>())
                as libc::mach_msg_type_number_t;
            let rc = libc::thread_info(
                self.port,
                libc::THREAD_BASIC_INFO as libc::thread_flavor_t,
                info.as_mut_ptr() as libc::thread_info_t,
                &mut count,
            );
            if rc != libc::KERN_SUCCESS {
                return None;
            }
            let info = info.assume_init();
            let ms = (info.user_time.seconds as i128 + info.system_time.seconds as i128) * 1_000
                + (info.user_time.microseconds as i128 + info.system_time.microseconds as i128)
                    / 1_000;
            Some(ms.max(0) as u64)
        }
    }
}

/// Guard for per-execution TRUE CPU-time budget enforcement.
///
/// Spawns a shared-runtime watchdog task that polls the execution thread's CPU clock every
/// [`CPU_BUDGET_POLL_INTERVAL`]. When accumulated active-JS CPU time exceeds the
/// budget, it calls `v8::Isolate::terminate_execution()` and signals the
/// execution abort with [`crate::session::ExecutionAbortReason::CpuBudgetExceeded`].
/// A guest that mostly awaits/idles accrues little CPU time and is NOT killed.
///
/// Drop or call `cancel()` to stop the watchdog (execution completed normally).
pub(crate) struct CpuBudgetGuard {
    fired: Arc<AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[cfg(unix)]
impl CpuBudgetGuard {
    /// Spawn the CPU-budget watchdog.
    ///
    /// - `budget_ms`: TRUE CPU-time budget in milliseconds (active JS only)
    /// - `cpu_clock`: the execution thread's CPU clock (captured on that thread)
    /// - `isolate_handle`: V8 isolate handle for `terminate_execution()`
    /// - `execution_abort`: signalled with `CpuBudgetExceeded` when the budget is exhausted
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn new(
        runtime: &RuntimeContext,
        owner: Option<TaskOwner>,
        budget_ms: u32,
        cpu_clock: ThreadCpuClock,
        isolate_handle: v8::IsolateHandle,
        execution_abort: crate::session::SharedExecutionAbort,
    ) -> Result<Self, String> {
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        // Snapshot the thread's CPU time at arm so the budget measures CPU
        // consumed DURING this execution, not cumulative thread lifetime.
        let baseline_ms = cpu_clock.elapsed_ms().unwrap_or(0);
        let budget_ms = budget_ms as u64;

        // Sample CPU consumption into a registry gauge every poll so the operator
        // gets an edge-triggered ~80% approach warning BEFORE the budget is
        // exhausted and the isolate is terminated (the terminal edge is reported
        // separately by `warn_limit_exhausted`).
        let cpu_gauge = register_limit(TrackedLimit::V8CpuTimeMs, budget_ms as usize);

        let handle = spawn_timer(runtime, owner, async move {
            let start = tokio::time::Instant::now() + CPU_BUDGET_POLL_INTERVAL;
            let mut ticker = tokio::time::interval_at(start, CPU_BUDGET_POLL_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let used = cpu_clock
                    .elapsed_ms()
                    .unwrap_or(baseline_ms)
                    .saturating_sub(baseline_ms);
                cpu_gauge.observe_depth(used as usize);
                if used >= budget_ms {
                    fired_clone.store(true, Ordering::SeqCst);
                    isolate_handle.terminate_execution();
                    crate::session::signal_execution_abort(
                        &execution_abort,
                        crate::session::ExecutionAbortReason::CpuBudgetExceeded,
                    );
                    return;
                }
            }
        })
        .map_err(|error| format!("{CPU_BUDGET_GUARD_START_ERROR_CODE}: {error}"))?;

        Ok(CpuBudgetGuard {
            fired,
            task: Some(handle),
        })
    }

    /// Cancel the watchdog when execution completes normally.
    pub(crate) fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Check whether the CPU budget was exhausted.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn exceeded(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for CpuBudgetGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

// Non-unix fallback: there is no portable per-thread CPU clock, so the
// CPU-budget watchdog cannot be enforced. `current_thread_cpu_clock` returns
// `None`, which makes the session surface a clear "cannot enforce" error if a
// CPU budget is requested, rather than silently running uncapped.
#[cfg(not(unix))]
#[derive(Clone, Copy)]
pub(crate) struct ThreadCpuClock;

#[cfg(not(unix))]
pub(crate) fn current_thread_cpu_clock() -> Option<ThreadCpuClock> {
    None
}

#[cfg(not(unix))]
impl CpuBudgetGuard {
    pub(crate) fn new(
        _runtime: &RuntimeContext,
        _owner: Option<TaskOwner>,
        _budget_ms: u32,
        _cpu_clock: ThreadCpuClock,
        _isolate_handle: v8::IsolateHandle,
        _execution_abort: crate::session::SharedExecutionAbort,
    ) -> Result<Self, String> {
        Err(format!(
            "{CPU_BUDGET_GUARD_START_ERROR_CODE}: per-thread CPU clock not supported on this platform"
        ))
    }

    pub(crate) fn cancel(&mut self) {}

    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn exceeded(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

/// Guard for per-execution wall-clock timeout enforcement.
///
/// Spawns a timer task that calls `v8::Isolate::terminate_execution()`
/// and closes the active execution abort channel to unblock any channel-based
/// readers when the timeout elapses. Drop or call `cancel()` to prevent firing.
pub struct TimeoutGuard {
    /// Set to true when the timeout fired
    fired: Arc<AtomicBool>,
    /// Shared-runtime timer task
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TimeoutGuard {
    /// Spawn a timeout task on the injected process runtime.
    ///
    /// - `timeout_ms`: wall-clock time limit in milliseconds
    /// - `isolate_handle`: V8 isolate handle for `terminate_execution()`
    /// - `abort_tx`: dropped on timeout to unblock channel readers via `select!`
    pub(crate) fn new(
        runtime: &RuntimeContext,
        owner: Option<TaskOwner>,
        timeout_ms: u32,
        isolate_handle: v8::IsolateHandle,
        abort_tx: crossbeam_channel::Sender<()>,
    ) -> Result<Self, String> {
        Self::spawn(runtime, owner, timeout_ms, isolate_handle, move || {
            drop(abort_tx);
        })
    }

    /// Spawn a wall-clock backstop that signals the execution abort with
    /// [`crate::session::ExecutionAbortReason::WallClockTimedOut`] when the limit
    /// elapses. Unlike the CPU budget, this counts elapsed real time INCLUDING
    /// idle/await. Armed only when the operator opts in via
    /// `limits.jsRuntime.wallClockLimitMs`.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn with_execution_abort(
        runtime: &RuntimeContext,
        owner: Option<TaskOwner>,
        timeout_ms: u32,
        isolate_handle: v8::IsolateHandle,
        execution_abort: crate::session::SharedExecutionAbort,
    ) -> Result<Self, String> {
        Self::spawn(runtime, owner, timeout_ms, isolate_handle, move || {
            crate::session::signal_execution_abort(
                &execution_abort,
                crate::session::ExecutionAbortReason::WallClockTimedOut,
            );
        })
    }

    fn spawn(
        runtime: &RuntimeContext,
        owner: Option<TaskOwner>,
        timeout_ms: u32,
        isolate_handle: v8::IsolateHandle,
        on_timeout: impl FnOnce() + Send + 'static,
    ) -> Result<Self, String> {
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        // Emit an edge-triggered ~80% approach warning before the wall-clock
        // budget is exhausted and the isolate is terminated. Observing the gauge
        // once at the threshold reuses the registry's warn + host-forward path.
        let wall_gauge = register_limit(TrackedLimit::V8WallClockMs, timeout_ms as usize);
        let warn_at_ms =
            timeout_ms as u64 * agentos_bridge::queue_tracker::WARN_FILL_PERCENT as u64 / 100;

        let handle = spawn_timer(runtime, owner, async move {
            let start = tokio::time::Instant::now();
            let warn_at = start + Duration::from_millis(warn_at_ms);
            let deadline = start + Duration::from_millis(timeout_ms as u64);
            tokio::time::sleep_until(warn_at).await;
            wall_gauge.observe_depth(warn_at_ms as usize);
            tokio::time::sleep_until(deadline).await;
            fired_clone.store(true, Ordering::SeqCst);
            isolate_handle.terminate_execution();
            on_timeout();
        })
        .map_err(|error| format!("{TIMEOUT_GUARD_START_ERROR_CODE}: {error}"))?;

        Ok(TimeoutGuard {
            fired,
            task: Some(handle),
        })
    }

    /// Cancel the timeout when execution completes normally.
    pub fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Check if the timeout fired.
    pub fn timed_out(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

fn spawn_timer<F>(
    runtime: &RuntimeContext,
    owner: Option<TaskOwner>,
    future: F,
) -> Result<tokio::task::JoinHandle<()>, agentos_runtime::TaskSpawnError>
where
    F: Future<Output = ()> + Send + 'static,
{
    match owner {
        Some(owner) => runtime.spawn_owned(TaskClass::Timer, owner, |_| {}, future),
        None => runtime.spawn(TaskClass::Timer, future),
    }
}

impl Drop for TimeoutGuard {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn timeout_guard_cancel_before_fire() {
        // Timer set to 5 seconds, cancelled immediately — should not fire
        let (abort_tx, abort_rx) = crossbeam_channel::bounded::<()>(0);

        // Create a minimal V8 platform + isolate just for the handle
        // We avoid actual V8 in tests — use a different approach
        // Instead, test the cancellation logic without V8

        // We can't easily get a v8::IsolateHandle without V8 init,
        // so we test the TimeoutGuard flow via integration in execution::tests
        drop(abort_tx);
        drop(abort_rx);
    }

    #[test]
    fn timeout_guard_fires_on_expiry() {
        // Tested via V8 integration tests in execution::tests
        // This placeholder confirms the module compiles correctly
    }
}
