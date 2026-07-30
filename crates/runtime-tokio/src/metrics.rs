//! Fixed-cardinality runtime telemetry and bounded stderr fallback formatting.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::supervision::{TaskClass, TaskTerminalReason};

const ORDERING: Ordering = Ordering::Relaxed;
pub const TASK_CLASS_COUNT: usize = 10;
pub const TASK_TERMINAL_REASON_COUNT: usize = 4;
pub const MAX_FALLBACK_MESSAGE_BYTES: usize = 512;

pub const TASK_CLASSES: [TaskClass; TASK_CLASS_COUNT] = [
    TaskClass::Runtime,
    TaskClass::Dns,
    TaskClass::Socket,
    TaskClass::Listener,
    TaskClass::Udp,
    TaskClass::Tls,
    TaskClass::Http2,
    TaskClass::Timer,
    TaskClass::Vm,
    TaskClass::Plugin,
];

pub const TASK_TERMINAL_REASONS: [TaskTerminalReason; TASK_TERMINAL_REASON_COUNT] = [
    TaskTerminalReason::Completed,
    TaskTerminalReason::Cancelled,
    TaskTerminalReason::Failed,
    TaskTerminalReason::Panicked,
];

macro_rules! fixed_metric_enum {
    ($name:ident, $count:ident, [$($variant:ident),+ $(,)?]) => {
        #[repr(usize)]
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: [Self; $count] = [$(Self::$variant),+];

            pub const fn index(self) -> usize {
                self as usize
            }
        }
    };
}

pub const RESOURCE_METRIC_CLASS_COUNT: usize = 13;
fixed_metric_enum!(
    ResourceMetricClass,
    RESOURCE_METRIC_CLASS_COUNT,
    [
        ActiveVms,
        Capabilities,
        ReadyHandles,
        Sockets,
        Connections,
        Timers,
        Tasks,
        BridgeCalls,
        HandleCommands,
        AsyncCompletions,
        Datagrams,
        Http2Connections,
        Http2Streams,
    ]
);

pub const BUFFER_METRIC_CLASS_COUNT: usize = 8;
fixed_metric_enum!(
    BufferMetricClass,
    BUFFER_METRIC_CLASS_COUNT,
    [Kernel, Native, Tls, Http2, Bridge, Datagram, Executor, Guest]
);

pub const WAKE_METRIC_COUNT: usize = 4;
fixed_metric_enum!(
    WakeMetric,
    WAKE_METRIC_COUNT,
    [Attempted, Coalesced, Delivered, Rearmed]
);

pub const FAIRNESS_LEVEL_COUNT: usize = 6;
fixed_metric_enum!(
    FairnessLevel,
    FAIRNESS_LEVEL_COUNT,
    [
        Process,
        Vm,
        Capability,
        Http2Stream,
        BridgeCompletion,
        Signal
    ]
);

pub const COMPLETION_ANOMALY_COUNT: usize = 3;
fixed_metric_enum!(
    CompletionAnomaly,
    COMPLETION_ANOMALY_COUNT,
    [Stale, Duplicate, Late]
);

pub const CHANNEL_METRIC_CLASS_COUNT: usize = 10;
fixed_metric_enum!(
    ChannelMetricClass,
    CHANNEL_METRIC_CLASS_COUNT,
    [
        ReadyWake,
        HandleCommand,
        BridgeResponse,
        BridgeEvent,
        AsyncCompletion,
        Signal,
        Datagram,
        Http2,
        StdioIngress,
        StdioEgress,
    ]
);

pub const EXECUTOR_METRIC_CLASS_COUNT: usize = 3;
fixed_metric_enum!(
    ExecutorMetricClass,
    EXECUTOR_METRIC_CLASS_COUNT,
    [Runtime, Vm, Blocking]
);

pub const WATCHDOG_METRIC_COUNT: usize = 4;
fixed_metric_enum!(
    WatchdogMetric,
    WATCHDOG_METRIC_COUNT,
    [
        LongTaskPoll,
        NonYieldingTask,
        RuntimeWorkerStall,
        ExecutorStall
    ]
);

pub const FALLBACK_SEVERITY_COUNT: usize = 2;
fixed_metric_enum!(TelemetrySeverity, FALLBACK_SEVERITY_COUNT, [Warning, Fatal]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetryFallbackCode {
    ResourceLimit,
    Overloaded,
    SupervisedTaskExit,
    RuntimeWorkerStall,
    TelemetryUnavailable,
}

impl TelemetryFallbackCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ResourceLimit => "ERR_AGENTOS_RESOURCE_LIMIT",
            Self::Overloaded => "ERR_AGENTOS_OVERLOADED",
            Self::SupervisedTaskExit => "ERR_AGENTOS_SUPERVISED_TASK_EXIT",
            Self::RuntimeWorkerStall => "ERR_AGENTOS_RUNTIME_WORKER_STALL",
            Self::TelemetryUnavailable => "ERR_AGENTOS_TELEMETRY_UNAVAILABLE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TelemetrySubsystem {
    Runtime,
    Reactor,
    Bridge,
    Executor,
    Telemetry,
}

impl TelemetrySubsystem {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
            Self::Reactor => "reactor",
            Self::Bridge => "bridge",
            Self::Executor => "executor",
            Self::Telemetry => "telemetry",
        }
    }
}

impl TelemetrySeverity {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Fatal => "fatal",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GaugeSnapshot {
    pub current: usize,
    pub high_water: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TaskMetricSnapshot {
    pub active: usize,
    pub terminal: [u64; TASK_TERMINAL_REASON_COUNT],
}

impl TaskMetricSnapshot {
    pub fn terminal_count(self, reason: TaskTerminalReason) -> u64 {
        self.terminal[task_terminal_reason_index(reason)]
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadinessMetricSnapshot {
    pub current_size: usize,
    pub size_high_water: usize,
    pub age_samples: u64,
    pub total_oldest_age_micros: u64,
    pub max_oldest_age_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ChannelMetricSnapshot {
    pub count_high_water: usize,
    pub byte_high_water: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExecutorMetricSnapshot {
    pub active: GaugeSnapshot,
    pub queued: GaugeSnapshot,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WatchdogMetricSnapshot {
    pub events: u64,
    pub total_stall_micros: u64,
    pub max_stall_micros: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeMetricsSnapshot {
    pub resources: [GaugeSnapshot; RESOURCE_METRIC_CLASS_COUNT],
    pub buffers: [GaugeSnapshot; BUFFER_METRIC_CLASS_COUNT],
    pub tasks: [TaskMetricSnapshot; TASK_CLASS_COUNT],
    pub wakes: [u64; WAKE_METRIC_COUNT],
    pub readiness: ReadinessMetricSnapshot,
    pub fairness_yields: [u64; FAIRNESS_LEVEL_COUNT],
    pub completion_anomalies: [u64; COMPLETION_ANOMALY_COUNT],
    pub channels: [ChannelMetricSnapshot; CHANNEL_METRIC_CLASS_COUNT],
    pub executors: [ExecutorMetricSnapshot; EXECUTOR_METRIC_CLASS_COUNT],
    pub watchdogs: [WatchdogMetricSnapshot; WATCHDOG_METRIC_COUNT],
    pub stderr_fallbacks: [u64; FALLBACK_SEVERITY_COUNT],
}

impl RuntimeMetricsSnapshot {
    pub fn task(&self, class: TaskClass) -> TaskMetricSnapshot {
        self.tasks[task_class_index(class)]
    }
}

#[derive(Debug)]
struct AtomicGauge {
    current: AtomicUsize,
    high_water: AtomicUsize,
}

impl AtomicGauge {
    fn new() -> Self {
        Self {
            current: AtomicUsize::new(0),
            high_water: AtomicUsize::new(0),
        }
    }

    fn observe(&self, current: usize) {
        self.current.store(current, ORDERING);
        saturating_fetch_max_usize(&self.high_water, current);
    }

    fn snapshot(&self) -> GaugeSnapshot {
        GaugeSnapshot {
            current: self.current.load(ORDERING),
            high_water: self.high_water.load(ORDERING),
        }
    }
}

#[derive(Debug)]
struct AtomicTaskMetric {
    active: AtomicUsize,
    terminal: [AtomicU64; TASK_TERMINAL_REASON_COUNT],
}

impl AtomicTaskMetric {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            terminal: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

#[derive(Debug)]
struct AtomicReadinessMetric {
    size: AtomicGauge,
    age_samples: AtomicU64,
    total_oldest_age_micros: AtomicU64,
    max_oldest_age_micros: AtomicU64,
}

impl AtomicReadinessMetric {
    fn new() -> Self {
        Self {
            size: AtomicGauge::new(),
            age_samples: AtomicU64::new(0),
            total_oldest_age_micros: AtomicU64::new(0),
            max_oldest_age_micros: AtomicU64::new(0),
        }
    }
}

#[derive(Debug)]
struct AtomicChannelMetric {
    count_high_water: AtomicUsize,
    byte_high_water: AtomicUsize,
}

impl AtomicChannelMetric {
    fn new() -> Self {
        Self {
            count_high_water: AtomicUsize::new(0),
            byte_high_water: AtomicUsize::new(0),
        }
    }
}

#[derive(Debug)]
struct AtomicExecutorMetric {
    active: AtomicGauge,
    queued: AtomicGauge,
}

impl AtomicExecutorMetric {
    fn new() -> Self {
        Self {
            active: AtomicGauge::new(),
            queued: AtomicGauge::new(),
        }
    }
}

#[derive(Debug)]
struct AtomicWatchdogMetric {
    events: AtomicU64,
    total_stall_micros: AtomicU64,
    max_stall_micros: AtomicU64,
}

impl AtomicWatchdogMetric {
    fn new() -> Self {
        Self {
            events: AtomicU64::new(0),
            total_stall_micros: AtomicU64::new(0),
            max_stall_micros: AtomicU64::new(0),
        }
    }
}

#[derive(Debug)]
struct MetricsInner {
    resources: [AtomicGauge; RESOURCE_METRIC_CLASS_COUNT],
    buffers: [AtomicGauge; BUFFER_METRIC_CLASS_COUNT],
    tasks: [AtomicTaskMetric; TASK_CLASS_COUNT],
    wakes: [AtomicU64; WAKE_METRIC_COUNT],
    readiness: AtomicReadinessMetric,
    fairness_yields: [AtomicU64; FAIRNESS_LEVEL_COUNT],
    completion_anomalies: [AtomicU64; COMPLETION_ANOMALY_COUNT],
    channels: [AtomicChannelMetric; CHANNEL_METRIC_CLASS_COUNT],
    executors: [AtomicExecutorMetric; EXECUTOR_METRIC_CLASS_COUNT],
    watchdogs: [AtomicWatchdogMetric; WATCHDOG_METRIC_COUNT],
    stderr_fallbacks: [AtomicU64; FALLBACK_SEVERITY_COUNT],
}

impl MetricsInner {
    fn new() -> Self {
        Self {
            resources: std::array::from_fn(|_| AtomicGauge::new()),
            buffers: std::array::from_fn(|_| AtomicGauge::new()),
            tasks: std::array::from_fn(|_| AtomicTaskMetric::new()),
            wakes: std::array::from_fn(|_| AtomicU64::new(0)),
            readiness: AtomicReadinessMetric::new(),
            fairness_yields: std::array::from_fn(|_| AtomicU64::new(0)),
            completion_anomalies: std::array::from_fn(|_| AtomicU64::new(0)),
            channels: std::array::from_fn(|_| AtomicChannelMetric::new()),
            executors: std::array::from_fn(|_| AtomicExecutorMetric::new()),
            watchdogs: std::array::from_fn(|_| AtomicWatchdogMetric::new()),
            stderr_fallbacks: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// Cloneable process-wide metrics handle. Its storage cardinality is fixed at construction.
#[derive(Clone, Debug)]
pub struct RuntimeMetrics {
    inner: Arc<MetricsInner>,
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeMetrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(MetricsInner::new()),
        }
    }

    pub fn observe_resource(&self, class: ResourceMetricClass, current: usize) {
        self.inner.resources[class.index()].observe(current);
    }

    pub fn observe_buffer(&self, class: BufferMetricClass, current_bytes: usize) {
        self.inner.buffers[class.index()].observe(current_bytes);
    }

    pub fn task_started(&self, class: TaskClass) {
        saturating_add_usize(&self.inner.tasks[task_class_index(class)].active, 1);
    }

    pub fn task_finished(&self, class: TaskClass, reason: TaskTerminalReason) {
        saturating_sub_usize(&self.inner.tasks[task_class_index(class)].active, 1);
        saturating_add_u64(
            &self.inner.tasks[task_class_index(class)].terminal[task_terminal_reason_index(reason)],
            1,
        );
    }

    pub fn record_wake(&self, metric: WakeMetric) {
        saturating_add_u64(&self.inner.wakes[metric.index()], 1);
    }

    pub fn observe_readiness(&self, ready_size: usize, oldest_age: Duration) {
        self.inner.readiness.size.observe(ready_size);
        let age_micros = duration_micros(oldest_age);
        saturating_add_u64(&self.inner.readiness.age_samples, 1);
        saturating_add_u64(&self.inner.readiness.total_oldest_age_micros, age_micros);
        saturating_fetch_max_u64(&self.inner.readiness.max_oldest_age_micros, age_micros);
    }

    pub fn record_fairness_yield(&self, level: FairnessLevel) {
        saturating_add_u64(&self.inner.fairness_yields[level.index()], 1);
    }

    pub fn record_completion_anomaly(&self, anomaly: CompletionAnomaly) {
        saturating_add_u64(&self.inner.completion_anomalies[anomaly.index()], 1);
    }

    pub fn observe_channel(
        &self,
        class: ChannelMetricClass,
        current_count: usize,
        current_bytes: usize,
    ) {
        let channel = &self.inner.channels[class.index()];
        saturating_fetch_max_usize(&channel.count_high_water, current_count);
        saturating_fetch_max_usize(&channel.byte_high_water, current_bytes);
    }

    pub fn observe_executor(&self, class: ExecutorMetricClass, active: usize, queued: usize) {
        let executor = &self.inner.executors[class.index()];
        executor.active.observe(active);
        executor.queued.observe(queued);
    }

    pub fn record_watchdog(&self, metric: WatchdogMetric, stall: Duration) {
        let watchdog = &self.inner.watchdogs[metric.index()];
        let micros = duration_micros(stall);
        saturating_add_u64(&watchdog.events, 1);
        saturating_add_u64(&watchdog.total_stall_micros, micros);
        saturating_fetch_max_u64(&watchdog.max_stall_micros, micros);
    }

    pub fn record_stderr_fallback(&self, severity: TelemetrySeverity) {
        saturating_add_u64(&self.inner.stderr_fallbacks[severity.index()], 1);
    }

    pub fn emit_stderr_fallback(&self, fallback: TelemetryFallback<'_>) {
        self.record_stderr_fallback(fallback.severity);
        emit_stderr_fallback(fallback);
    }

    /// Atomics make this snapshot race-safe but intentionally not transactional.
    pub fn snapshot(&self) -> RuntimeMetricsSnapshot {
        RuntimeMetricsSnapshot {
            resources: std::array::from_fn(|index| self.inner.resources[index].snapshot()),
            buffers: std::array::from_fn(|index| self.inner.buffers[index].snapshot()),
            tasks: std::array::from_fn(|index| TaskMetricSnapshot {
                active: self.inner.tasks[index].active.load(ORDERING),
                terminal: std::array::from_fn(|reason| {
                    self.inner.tasks[index].terminal[reason].load(ORDERING)
                }),
            }),
            wakes: std::array::from_fn(|index| self.inner.wakes[index].load(ORDERING)),
            readiness: ReadinessMetricSnapshot {
                current_size: self.inner.readiness.size.current.load(ORDERING),
                size_high_water: self.inner.readiness.size.high_water.load(ORDERING),
                age_samples: self.inner.readiness.age_samples.load(ORDERING),
                total_oldest_age_micros: self
                    .inner
                    .readiness
                    .total_oldest_age_micros
                    .load(ORDERING),
                max_oldest_age_micros: self.inner.readiness.max_oldest_age_micros.load(ORDERING),
            },
            fairness_yields: std::array::from_fn(|index| {
                self.inner.fairness_yields[index].load(ORDERING)
            }),
            completion_anomalies: std::array::from_fn(|index| {
                self.inner.completion_anomalies[index].load(ORDERING)
            }),
            channels: std::array::from_fn(|index| ChannelMetricSnapshot {
                count_high_water: self.inner.channels[index].count_high_water.load(ORDERING),
                byte_high_water: self.inner.channels[index].byte_high_water.load(ORDERING),
            }),
            executors: std::array::from_fn(|index| ExecutorMetricSnapshot {
                active: self.inner.executors[index].active.snapshot(),
                queued: self.inner.executors[index].queued.snapshot(),
            }),
            watchdogs: std::array::from_fn(|index| WatchdogMetricSnapshot {
                events: self.inner.watchdogs[index].events.load(ORDERING),
                total_stall_micros: self.inner.watchdogs[index]
                    .total_stall_micros
                    .load(ORDERING),
                max_stall_micros: self.inner.watchdogs[index].max_stall_micros.load(ORDERING),
            }),
            stderr_fallbacks: std::array::from_fn(|index| {
                self.inner.stderr_fallbacks[index].load(ORDERING)
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TelemetryFallback<'a> {
    pub severity: TelemetrySeverity,
    pub code: TelemetryFallbackCode,
    pub subsystem: TelemetrySubsystem,
    pub message: &'a str,
}

pub fn format_stderr_fallback(fallback: TelemetryFallback<'_>) -> String {
    let message_end = floor_char_boundary(fallback.message, MAX_FALLBACK_MESSAGE_BYTES);
    let truncated = message_end < fallback.message.len();
    let mut message = String::with_capacity(message_end);
    for character in fallback.message[..message_end].chars() {
        message.push(match character {
            '"' => '\'',
            '\\' => '/',
            character if character.is_control() => ' ',
            character => character,
        });
    }
    format!(
        "AGENTOS_TELEMETRY_FALLBACK severity={} code={} subsystem={} message=\"{}\" truncated={truncated}",
        fallback.severity.as_str(),
        fallback.code.as_str(),
        fallback.subsystem.as_str(),
        message,
    )
}

pub fn emit_stderr_fallback(fallback: TelemetryFallback<'_>) {
    eprintln!("{}", format_stderr_fallback(fallback));
}

fn task_class_index(class: TaskClass) -> usize {
    match class {
        TaskClass::Runtime => 0,
        TaskClass::Dns => 1,
        TaskClass::Socket => 2,
        TaskClass::Listener => 3,
        TaskClass::Udp => 4,
        TaskClass::Tls => 5,
        TaskClass::Http2 => 6,
        TaskClass::Timer => 7,
        TaskClass::Vm => 8,
        TaskClass::Plugin => 9,
    }
}

fn task_terminal_reason_index(reason: TaskTerminalReason) -> usize {
    match reason {
        TaskTerminalReason::Completed => 0,
        TaskTerminalReason::Cancelled => 1,
        TaskTerminalReason::Failed => 2,
        TaskTerminalReason::Panicked => 3,
    }
}

fn duration_micros(duration: Duration) -> u64 {
    duration.as_micros().min(u128::from(u64::MAX)) as u64
}

fn floor_char_boundary(value: &str, maximum_bytes: usize) -> usize {
    let mut end = value.len().min(maximum_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn saturating_add_u64(value: &AtomicU64, amount: u64) {
    let mut current = value.load(ORDERING);
    loop {
        let next = current.saturating_add(amount);
        if next == current {
            return;
        }
        match value.compare_exchange_weak(current, next, ORDERING, ORDERING) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_add_usize(value: &AtomicUsize, amount: usize) {
    let mut current = value.load(ORDERING);
    loop {
        let next = current.saturating_add(amount);
        if next == current {
            return;
        }
        match value.compare_exchange_weak(current, next, ORDERING, ORDERING) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_sub_usize(value: &AtomicUsize, amount: usize) {
    let mut current = value.load(ORDERING);
    loop {
        let next = current.saturating_sub(amount);
        if next == current {
            return;
        }
        match value.compare_exchange_weak(current, next, ORDERING, ORDERING) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_fetch_max_u64(value: &AtomicU64, observed: u64) {
    let mut current = value.load(ORDERING);
    while observed > current {
        match value.compare_exchange_weak(current, observed, ORDERING, ORDERING) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

fn saturating_fetch_max_usize(value: &AtomicUsize, observed: usize) {
    let mut current = value.load(ORDERING);
    while observed > current {
        match value.compare_exchange_weak(current, observed, ORDERING, ORDERING) {
            Ok(_) => return,
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_cardinality_is_fixed_by_enums() {
        let metrics = RuntimeMetrics::new();
        for class in ResourceMetricClass::ALL {
            metrics.observe_resource(class, class.index() + 1);
        }
        for class in BufferMetricClass::ALL {
            metrics.observe_buffer(class, class.index() + 1);
        }
        for class in TASK_CLASSES {
            metrics.task_started(class);
            for reason in TASK_TERMINAL_REASONS {
                metrics.task_finished(class, reason);
            }
        }
        for metric in WakeMetric::ALL {
            metrics.record_wake(metric);
        }
        for level in FairnessLevel::ALL {
            metrics.record_fairness_yield(level);
        }
        for anomaly in CompletionAnomaly::ALL {
            metrics.record_completion_anomaly(anomaly);
        }
        for class in ChannelMetricClass::ALL {
            metrics.observe_channel(class, 1, 1);
        }
        for class in ExecutorMetricClass::ALL {
            metrics.observe_executor(class, 1, 1);
        }
        for metric in WatchdogMetric::ALL {
            metrics.record_watchdog(metric, Duration::from_micros(1));
        }

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.resources.len(), RESOURCE_METRIC_CLASS_COUNT);
        assert_eq!(snapshot.buffers.len(), BUFFER_METRIC_CLASS_COUNT);
        assert_eq!(snapshot.tasks.len(), TASK_CLASS_COUNT);
        assert!(snapshot
            .tasks
            .iter()
            .all(|task| task.terminal.len() == TASK_TERMINAL_REASON_COUNT));
        assert_eq!(snapshot.wakes.len(), WAKE_METRIC_COUNT);
        assert_eq!(snapshot.fairness_yields.len(), FAIRNESS_LEVEL_COUNT);
        assert_eq!(
            snapshot.completion_anomalies.len(),
            COMPLETION_ANOMALY_COUNT
        );
        assert_eq!(snapshot.channels.len(), CHANNEL_METRIC_CLASS_COUNT);
        assert_eq!(snapshot.executors.len(), EXECUTOR_METRIC_CLASS_COUNT);
        assert_eq!(snapshot.watchdogs.len(), WATCHDOG_METRIC_COUNT);
        assert_eq!(snapshot.stderr_fallbacks.len(), FALLBACK_SEVERITY_COUNT);
    }

    #[test]
    fn high_water_marks_do_not_fall_and_dimensions_are_independent() {
        let metrics = RuntimeMetrics::new();
        metrics.observe_channel(ChannelMetricClass::BridgeResponse, 5, 100);
        metrics.observe_channel(ChannelMetricClass::BridgeResponse, 3, 200);
        metrics.observe_channel(ChannelMetricClass::BridgeResponse, 7, 150);
        metrics.observe_executor(ExecutorMetricClass::Vm, 4, 9);
        metrics.observe_executor(ExecutorMetricClass::Vm, 2, 3);
        metrics.observe_readiness(8, Duration::from_micros(20));
        metrics.observe_readiness(3, Duration::from_micros(50));

        let snapshot = metrics.snapshot();
        assert_eq!(
            snapshot.channels[ChannelMetricClass::BridgeResponse.index()],
            ChannelMetricSnapshot {
                count_high_water: 7,
                byte_high_water: 200,
            }
        );
        assert_eq!(
            snapshot.executors[ExecutorMetricClass::Vm.index()],
            ExecutorMetricSnapshot {
                active: GaugeSnapshot {
                    current: 2,
                    high_water: 4,
                },
                queued: GaugeSnapshot {
                    current: 3,
                    high_water: 9,
                },
            }
        );
        assert_eq!(snapshot.readiness.current_size, 3);
        assert_eq!(snapshot.readiness.size_high_water, 8);
        assert_eq!(snapshot.readiness.age_samples, 2);
        assert_eq!(snapshot.readiness.total_oldest_age_micros, 70);
        assert_eq!(snapshot.readiness.max_oldest_age_micros, 50);
    }

    #[test]
    fn counters_saturate_instead_of_wrapping() {
        let metrics = RuntimeMetrics::new();
        metrics.inner.wakes[WakeMetric::Attempted.index()].store(u64::MAX - 1, ORDERING);
        metrics.record_wake(WakeMetric::Attempted);
        metrics.record_wake(WakeMetric::Attempted);
        metrics.inner.tasks[task_class_index(TaskClass::Runtime)]
            .active
            .store(usize::MAX, ORDERING);
        metrics.task_started(TaskClass::Runtime);
        metrics
            .inner
            .readiness
            .total_oldest_age_micros
            .store(u64::MAX - 1, ORDERING);
        metrics.observe_readiness(0, Duration::from_micros(2));

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.wakes[WakeMetric::Attempted.index()], u64::MAX);
        assert_eq!(
            snapshot.tasks[task_class_index(TaskClass::Runtime)].active,
            usize::MAX
        );
        assert_eq!(snapshot.readiness.total_oldest_age_micros, u64::MAX);
    }

    #[test]
    fn fallback_format_is_compact_bounded_and_single_line() {
        let formatted = format_stderr_fallback(TelemetryFallback {
            severity: TelemetrySeverity::Warning,
            code: TelemetryFallbackCode::ResourceLimit,
            subsystem: TelemetrySubsystem::Reactor,
            message: "queue \"full\"\nretry",
        });
        assert_eq!(
            formatted,
            "AGENTOS_TELEMETRY_FALLBACK severity=warning code=ERR_AGENTOS_RESOURCE_LIMIT subsystem=reactor message=\"queue 'full' retry\" truncated=false"
        );
        assert!(!formatted.contains('\n'));

        let oversized = "x".repeat(MAX_FALLBACK_MESSAGE_BYTES + 100);
        let truncated = format_stderr_fallback(TelemetryFallback {
            severity: TelemetrySeverity::Fatal,
            code: TelemetryFallbackCode::RuntimeWorkerStall,
            subsystem: TelemetrySubsystem::Runtime,
            message: &oversized,
        });
        assert!(truncated.ends_with("truncated=true"));
        assert!(truncated.len() < MAX_FALLBACK_MESSAGE_BYTES + 200);
    }
}
