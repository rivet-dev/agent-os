//! Bounded task ownership, terminal accounting, and owner notification.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::accounting::{LimitError, Reservation, ResourceClass, ResourceLedger};
use crate::metrics::{
    RuntimeMetrics, TelemetryFallback, TelemetryFallbackCode, TelemetrySeverity, TelemetrySubsystem,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TaskClass {
    Runtime,
    Dns,
    Socket,
    Listener,
    Udp,
    Tls,
    Http2,
    Timer,
    Vm,
    Plugin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskOwner {
    Process,
    Vm { generation: u64 },
    Capability { id: u64, generation: u64 },
    Connection { id: u64, generation: u64 },
    Background { name: &'static str },
}

impl fmt::Display for TaskOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Process => formatter.write_str("process"),
            Self::Vm { generation } => write!(formatter, "vm-generation={generation}"),
            Self::Capability { id, generation } => {
                write!(formatter, "capability={id} generation={generation}")
            }
            Self::Connection { id, generation } => {
                write!(formatter, "connection={id} generation={generation}")
            }
            Self::Background { name } => write!(formatter, "background={name}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TaskTerminalReason {
    Completed,
    Cancelled,
    Failed,
    Panicked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskTerminalReport {
    pub class: TaskClass,
    pub owner: TaskOwner,
    pub scope: String,
    pub reason: TaskTerminalReason,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TaskClassSnapshot {
    pub active: usize,
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub panicked: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TaskSpawnError {
    ResourceLimit(LimitError),
    AdmissionClosed { scope: String },
}

impl fmt::Display for TaskSpawnError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimit(error) => error.fmt(formatter),
            Self::AdmissionClosed { scope } => write!(
                formatter,
                "ERR_AGENTOS_TASK_ADMISSION_CLOSED: scope={scope} is closing; new task admission is disabled"
            ),
        }
    }
}

impl std::error::Error for TaskSpawnError {}

impl From<LimitError> for TaskSpawnError {
    fn from(error: LimitError) -> Self {
        Self::ResourceLimit(error)
    }
}

impl TaskClassSnapshot {
    fn record(&mut self, reason: TaskTerminalReason) {
        let counter = match reason {
            TaskTerminalReason::Completed => &mut self.completed,
            TaskTerminalReason::Cancelled => &mut self.cancelled,
            TaskTerminalReason::Failed => &mut self.failed,
            TaskTerminalReason::Panicked => &mut self.panicked,
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Debug, Default)]
struct TaskSupervisorState {
    classes: BTreeMap<TaskClass, TaskClassSnapshot>,
    active_scopes: BTreeMap<String, usize>,
    reports: VecDeque<TaskTerminalReport>,
    dropped_reports: u64,
    report_overflow_warned: bool,
}

type TerminalHandler = Arc<dyn Fn(&TaskTerminalReport) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct TaskSupervisor {
    ledger: Arc<ResourceLedger>,
    metrics: RuntimeMetrics,
    state: Arc<Mutex<TaskSupervisorState>>,
    settled: Arc<tokio::sync::Notify>,
    admission_open: Arc<AtomicBool>,
    admission_gate: Arc<Mutex<()>>,
    report_capacity: usize,
}

impl TaskSupervisor {
    pub(crate) fn new(
        ledger: Arc<ResourceLedger>,
        metrics: RuntimeMetrics,
        admission_open: Arc<AtomicBool>,
        admission_gate: Arc<Mutex<()>>,
        report_capacity: usize,
    ) -> Self {
        Self::with_report_capacity(
            ledger,
            metrics,
            admission_open,
            admission_gate,
            report_capacity,
        )
    }

    fn with_report_capacity(
        ledger: Arc<ResourceLedger>,
        metrics: RuntimeMetrics,
        admission_open: Arc<AtomicBool>,
        admission_gate: Arc<Mutex<()>>,
        report_capacity: usize,
    ) -> Self {
        Self {
            ledger,
            metrics,
            state: Arc::new(Mutex::new(TaskSupervisorState::default())),
            settled: Arc::new(tokio::sync::Notify::new()),
            admission_open,
            admission_gate,
            report_capacity: report_capacity.max(1),
        }
    }

    pub(crate) fn admit(
        &self,
        class: TaskClass,
        owner: TaskOwner,
        handler: Option<TerminalHandler>,
    ) -> Result<TaskGuard, TaskSpawnError> {
        // Linearize insertion against close so teardown cannot observe an
        // empty scope and then have a stale clone insert a new task.
        let _admission = self.admission_gate.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_TASK_ADMISSION_GATE_POISONED: recovering task admission");
            poisoned.into_inner()
        });
        self.ensure_admission_open()?;
        let reservation = self
            .ledger
            .reserve(ResourceClass::Tasks, 1)
            .map_err(TaskSpawnError::ResourceLimit)?;
        let scope = self.ledger.scope().to_owned();
        let mut state = self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_TASK_SUPERVISOR_POISONED: recovering task admission");
            poisoned.into_inner()
        });
        state.classes.entry(class).or_default().active += 1;
        *state.active_scopes.entry(scope.clone()).or_default() += 1;
        drop(state);
        self.metrics.task_started(class);
        Ok(TaskGuard {
            class,
            owner,
            scope,
            supervisor: self.clone(),
            handler,
            reservation: Some(reservation),
            terminal: None,
        })
    }

    pub fn snapshot(&self, class: TaskClass) -> TaskClassSnapshot {
        self.state
            .lock()
            .map(|state| state.classes.get(&class).copied().unwrap_or_default())
            .unwrap_or_else(|_| {
                eprintln!("ERR_AGENTOS_TASK_SUPERVISOR_POISONED: failed to read task census");
                TaskClassSnapshot::default()
            })
    }

    pub fn active_total(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.classes.values().map(|stats| stats.active).sum())
            .unwrap_or_else(|_| {
                eprintln!("ERR_AGENTOS_TASK_SUPERVISOR_POISONED: failed to read active census");
                0
            })
    }

    pub fn active_scoped(&self) -> usize {
        let scope = self.ledger.scope();
        self.state
            .lock()
            .map(|state| state.active_scopes.get(scope).copied().unwrap_or(0))
            .unwrap_or_else(|_| {
                eprintln!(
                    "ERR_AGENTOS_TASK_SUPERVISOR_POISONED: failed to read scoped task census"
                );
                usize::MAX
            })
    }

    /// Wait until this RuntimeContext's accounting scope owns no supervised
    /// tasks. The notification is armed before observation so the final task
    /// cannot exit between the check and await.
    pub async fn wait_empty(&self) {
        loop {
            let settled = self.settled.notified();
            if self.active_scoped() == 0 {
                return;
            }
            settled.await;
        }
    }

    pub(crate) fn close_admission(&self) {
        let _admission = self.admission_gate.lock().unwrap_or_else(|poisoned| {
            eprintln!("ERR_AGENTOS_TASK_ADMISSION_GATE_POISONED: recovering task close");
            poisoned.into_inner()
        });
        self.admission_open.store(false, Ordering::Release);
    }

    fn ensure_admission_open(&self) -> Result<(), TaskSpawnError> {
        if self.admission_open.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(TaskSpawnError::AdmissionClosed {
                scope: self.ledger.scope().to_owned(),
            })
        }
    }

    pub fn drain_terminal_reports(&self) -> Vec<TaskTerminalReport> {
        self.state
            .lock()
            .map(|mut state| {
                state.report_overflow_warned = false;
                state.reports.drain(..).collect()
            })
            .unwrap_or_else(|_| {
                eprintln!("ERR_AGENTOS_TASK_SUPERVISOR_POISONED: failed to drain terminal reports");
                Vec::new()
            })
    }

    pub fn dropped_terminal_reports(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.dropped_reports)
            .unwrap_or(u64::MAX)
    }

    pub(crate) fn scoped(
        &self,
        ledger: Arc<ResourceLedger>,
        admission_open: Arc<AtomicBool>,
        admission_gate: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            ledger,
            metrics: self.metrics.clone(),
            state: Arc::clone(&self.state),
            settled: Arc::clone(&self.settled),
            admission_open,
            admission_gate,
            report_capacity: self.report_capacity,
        }
    }

    fn terminal(&self, report: TaskTerminalReport, handler: Option<&TerminalHandler>) {
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| {
                eprintln!("ERR_AGENTOS_TASK_SUPERVISOR_POISONED: recovering terminal task report");
                poisoned.into_inner()
            });
            let stats = state.classes.entry(report.class).or_default();
            if stats.active == 0 {
                eprintln!(
                    "ERR_AGENTOS_TASK_ACCOUNTING_UNDERFLOW: class={:?} owner={}",
                    report.class, report.owner
                );
            } else {
                stats.active -= 1;
            }
            stats.record(report.reason);
            match state.active_scopes.get_mut(&report.scope) {
                Some(active) if *active > 1 => *active -= 1,
                Some(_) => {
                    state.active_scopes.remove(&report.scope);
                }
                None => eprintln!(
                    "ERR_AGENTOS_TASK_ACCOUNTING_UNDERFLOW: scope={} class={:?} owner={}",
                    report.scope, report.class, report.owner
                ),
            }
            // Normal completion and owner-driven cancellation are already
            // handled by the class census, low-cardinality metrics, and the
            // terminal callback. Retaining every successful report would make
            // a healthy long-lived process inevitably fill this diagnostic
            // buffer even though no failure was unobserved.
            if matches!(
                report.reason,
                TaskTerminalReason::Failed | TaskTerminalReason::Panicked
            ) {
                if state.reports.len() == self.report_capacity {
                    state.reports.pop_front();
                    state.dropped_reports = state.dropped_reports.saturating_add(1);
                    if !state.report_overflow_warned {
                        state.report_overflow_warned = true;
                        eprintln!(
                            "ERR_AGENTOS_TASK_REPORT_LIMIT: failed task report buffer exceeded {}; latest_class={:?} latest_owner={}; oldest reports will be dropped until drained; raise runtime.tasks.maxTerminalReports",
                            self.report_capacity, report.class, report.owner
                        );
                    }
                }
                state.reports.push_back(report.clone());
            }
        }
        self.metrics.task_finished(report.class, report.reason);
        self.settled.notify_waiters();

        if matches!(
            report.reason,
            TaskTerminalReason::Failed | TaskTerminalReason::Panicked
        ) {
            let message = format!(
                "class={:?} owner={} reason={:?}",
                report.class, report.owner, report.reason
            );
            self.metrics.emit_stderr_fallback(TelemetryFallback {
                severity: TelemetrySeverity::Fatal,
                code: TelemetryFallbackCode::SupervisedTaskExit,
                subsystem: TelemetrySubsystem::Runtime,
                message: &message,
            });
        }
        if let Some(handler) = handler {
            if catch_unwind(AssertUnwindSafe(|| handler(&report))).is_err() {
                eprintln!(
                    "ERR_AGENTOS_TASK_TERMINAL_HANDLER_PANIC: class={:?} owner={} reason={:?}",
                    report.class, report.owner, report.reason
                );
            }
        }
    }
}

pub(crate) struct TaskGuard {
    class: TaskClass,
    owner: TaskOwner,
    scope: String,
    supervisor: TaskSupervisor,
    handler: Option<TerminalHandler>,
    reservation: Option<Reservation>,
    terminal: Option<TaskTerminalReason>,
}

impl TaskGuard {
    pub(crate) fn complete(&mut self) {
        self.terminal = Some(TaskTerminalReason::Completed);
    }

    pub(crate) fn fail(&mut self) {
        self.terminal = Some(TaskTerminalReason::Failed);
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let reason = if std::thread::panicking() {
            TaskTerminalReason::Panicked
        } else {
            self.terminal.unwrap_or(TaskTerminalReason::Cancelled)
        };
        let report = TaskTerminalReport {
            class: self.class,
            owner: self.owner.clone(),
            scope: self.scope.clone(),
            reason,
        };
        // Release task admission before publishing terminal completion. A
        // waiter that observes an empty scope must also observe zero task
        // reservations in the VM ledger.
        self.reservation.take();
        self.supervisor.terminal(report, self.handler.as_ref());
    }
}

pub(crate) fn terminal_handler(
    handler: impl Fn(&TaskTerminalReport) + Send + Sync + 'static,
) -> TerminalHandler {
    Arc::new(handler)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::ResourceLimit;

    fn supervisor(report_capacity: usize) -> TaskSupervisor {
        let ledger = Arc::new(ResourceLedger::root(
            "process",
            [(
                ResourceClass::Tasks,
                ResourceLimit::new(4, "runtime.resources.maxTasks"),
            )],
        ));
        TaskSupervisor::with_report_capacity(
            ledger,
            RuntimeMetrics::new(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(())),
            report_capacity,
        )
    }

    #[test]
    fn failed_reports_are_bounded_and_metrics_are_low_cardinality() {
        let supervisor = supervisor(2);
        for generation in 1..=4 {
            let mut guard = supervisor
                .admit(TaskClass::Vm, TaskOwner::Vm { generation }, None)
                .expect("admit");
            guard.fail();
        }
        assert_eq!(supervisor.snapshot(TaskClass::Vm).failed, 4);
        assert_eq!(supervisor.dropped_terminal_reports(), 2);
        assert!(
            supervisor
                .state
                .lock()
                .expect("supervisor state")
                .report_overflow_warned,
            "repeated drops should retain one edge-triggered warning state"
        );
        let reports = supervisor.drain_terminal_reports();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].owner, TaskOwner::Vm { generation: 3 });
        assert!(
            !supervisor
                .state
                .lock()
                .expect("supervisor state")
                .report_overflow_warned,
            "draining reports should re-arm the next overflow warning"
        );
    }

    #[test]
    fn successful_and_cancelled_tasks_do_not_consume_failure_report_capacity() {
        let supervisor = supervisor(1);
        for generation in 1..=4 {
            let mut completed = supervisor
                .admit(TaskClass::Vm, TaskOwner::Vm { generation }, None)
                .expect("admit completed task");
            completed.complete();

            let cancelled = supervisor
                .admit(TaskClass::Vm, TaskOwner::Vm { generation }, None)
                .expect("admit cancelled task");
            drop(cancelled);
        }

        let snapshot = supervisor.snapshot(TaskClass::Vm);
        assert_eq!(snapshot.completed, 4);
        assert_eq!(snapshot.cancelled, 4);
        assert_eq!(supervisor.dropped_terminal_reports(), 0);
        assert!(supervisor.drain_terminal_reports().is_empty());
    }

    #[test]
    fn owner_handler_runs_after_terminal_accounting() {
        let supervisor = supervisor(2);
        let observed = Arc::new(Mutex::new(None));
        let observed_for_handler = Arc::clone(&observed);
        let handler = terminal_handler(move |report| {
            *observed_for_handler.lock().expect("handler state") = Some(report.clone());
        });
        let guard = supervisor
            .admit(
                TaskClass::Socket,
                TaskOwner::Capability {
                    id: 9,
                    generation: 4,
                },
                Some(handler),
            )
            .expect("admit");
        drop(guard);
        assert_eq!(supervisor.snapshot(TaskClass::Socket).cancelled, 1);
        assert_eq!(
            observed.lock().expect("observed").as_ref().unwrap().reason,
            TaskTerminalReason::Cancelled
        );
    }

    #[tokio::test]
    async fn scoped_wait_ignores_other_scopes_and_cannot_miss_final_exit() {
        let process = Arc::new(ResourceLedger::root(
            "process",
            [(
                ResourceClass::Tasks,
                ResourceLimit::new(4, "runtime.resources.maxTasks"),
            )],
        ));
        let base = TaskSupervisor::new(
            Arc::clone(&process),
            RuntimeMetrics::new(),
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(())),
            4_096,
        );
        let vm_1_ledger = Arc::new(ResourceLedger::child(
            "vm=1 generation=1",
            [(
                ResourceClass::Tasks,
                ResourceLimit::new(2, "limits.reactor.maxTasks"),
            )],
            Arc::clone(&process),
        ));
        let vm_2_ledger = Arc::new(ResourceLedger::child(
            "vm=2 generation=1",
            [(
                ResourceClass::Tasks,
                ResourceLimit::new(2, "limits.reactor.maxTasks"),
            )],
            Arc::clone(&process),
        ));
        let vm_1 = base.scoped(
            Arc::clone(&vm_1_ledger),
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(())),
        );
        let vm_2 = base.scoped(
            Arc::clone(&vm_2_ledger),
            Arc::new(AtomicBool::new(true)),
            Arc::new(Mutex::new(())),
        );
        let vm_1_guard = vm_1
            .admit(TaskClass::Socket, TaskOwner::Vm { generation: 1 }, None)
            .expect("vm 1 task");
        let _vm_2_guard = vm_2
            .admit(TaskClass::Socket, TaskOwner::Vm { generation: 1 }, None)
            .expect("vm 2 task");
        let waiter = tokio::spawn({
            let vm_1 = vm_1.clone();
            async move { vm_1.wait_empty().await }
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        drop(vm_1_guard);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("vm 1 waiter")
            .expect("wait task");
        assert_eq!(vm_1.active_scoped(), 0);
        assert_eq!(vm_2.active_scoped(), 1);
        assert_eq!(vm_1_ledger.usage(ResourceClass::Tasks).used, 0);
    }
}
