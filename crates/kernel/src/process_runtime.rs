use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

const MAX_RUNTIME_FAULT_CODE_BYTES: usize = 128;
const MAX_RUNTIME_FAULT_MESSAGE_BYTES: usize = 4 * 1024;
const MAX_RUNTIME_FAULT_DETAILS_BYTES: usize = 64 * 1024;

/// Identifies the one VM generation and kernel process an endpoint may control.
///
/// The generation is allocated by the sidecar. It prevents a retained endpoint
/// from being reused after a VM id is destroyed and recreated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessRuntimeIdentity {
    pub generation: u64,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessTermination {
    Signal { signal: i32, force: bool },
    RuntimeFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessCancellationReason {
    VmTeardown,
    Deadline,
    HostRequest,
    RuntimeFault,
}

/// Exact terminal state reported by an execution. The kernel never infers a
/// signal from an exit code such as `128 + signal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExit {
    Exited(i32),
    Signaled { signal: i32, core_dumped: bool },
}

/// Stable, bounded diagnostic for an executor failure that is not a guest
/// `exit(2)` or signal termination.
///
/// The kernel keeps this beside the synthetic Linux exit status used by
/// `waitpid`; callers never parse an engine string to recover its category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRuntimeFault {
    code: String,
    message: String,
    details: Option<Value>,
}

impl ProcessRuntimeFault {
    pub fn try_new(
        code: impl Into<String>,
        message: impl Into<String>,
        details: Option<Value>,
    ) -> Result<Self, ProcessRuntimeEndpointError> {
        let code = code.into();
        let message = message.into();
        if code.is_empty() {
            return Err(ProcessRuntimeEndpointError::new(
                "EINVAL",
                "runtime fault code must not be empty",
            ));
        }
        if code.len() > MAX_RUNTIME_FAULT_CODE_BYTES {
            return Err(ProcessRuntimeEndpointError::new(
                "E2BIG",
                format!(
                    "runtime fault code is {} bytes; maximum is {MAX_RUNTIME_FAULT_CODE_BYTES}",
                    code.len()
                ),
            ));
        }
        if message.len() > MAX_RUNTIME_FAULT_MESSAGE_BYTES {
            return Err(ProcessRuntimeEndpointError::new(
                "E2BIG",
                format!(
                    "runtime fault message is {} bytes; maximum is {MAX_RUNTIME_FAULT_MESSAGE_BYTES}",
                    message.len()
                ),
            ));
        }
        if let Some(value) = &details {
            let mut counter = BoundedJsonCounter::new(MAX_RUNTIME_FAULT_DETAILS_BYTES);
            if let Err(error) = serde_json::to_writer(&mut counter, value) {
                if counter.exceeded {
                    return Err(ProcessRuntimeEndpointError::new(
                        "E2BIG",
                        format!(
                            "runtime fault details exceed {MAX_RUNTIME_FAULT_DETAILS_BYTES} bytes"
                        ),
                    ));
                }
                return Err(ProcessRuntimeEndpointError::new(
                    "EINVAL",
                    format!("runtime fault details are not encodable: {error}"),
                ));
            }
        }
        Ok(Self {
            code,
            message,
            details,
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn details(&self) -> Option<&Value> {
        self.details.as_ref()
    }
}

struct BoundedJsonCounter {
    written: usize,
    limit: usize,
    exceeded: bool,
}

impl BoundedJsonCounter {
    fn new(limit: usize) -> Self {
        Self {
            written: 0,
            limit,
            exceeded: false,
        }
    }
}

impl Write for BoundedJsonCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next) = self.written.checked_add(bytes.len()) else {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "runtime fault details exceed their encoded limit",
            ));
        };
        if next > self.limit {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "runtime fault details exceed their encoded limit",
            ));
        }
        self.written = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ProcessExit {
    pub fn shell_status(self) -> i32 {
        match self {
            Self::Exited(code) => code,
            Self::Signaled { signal, .. } => 128 + signal,
        }
    }
}

/// Runtime-neutral control requested by the kernel.
///
/// Requests update durable state only. An endpoint must never enter guest code
/// or wait for the runtime while servicing this call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessControlRequest {
    Checkpoint,
    Stop { signal: i32 },
    Continue,
    Terminate(ProcessTermination),
    Cancel(ProcessCancellationReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRuntimeEndpointError {
    code: &'static str,
    message: String,
}

impl ProcessRuntimeEndpointError {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ProcessRuntimeEndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for ProcessRuntimeEndpointError {}

pub(crate) trait ProcessExitSink: Send + Sync {
    fn report_exit(
        &self,
        identity: ProcessRuntimeIdentity,
        termination: ProcessExit,
    ) -> Result<(), ProcessRuntimeEndpointError>;

    fn report_runtime_fault(
        &self,
        identity: ProcessRuntimeIdentity,
        fault: ProcessRuntimeFault,
    ) -> Result<(), ProcessRuntimeEndpointError>;
}

pub(crate) trait ProcessControlAckSink: Send + Sync {
    fn acknowledge_stop_state(
        &self,
        identity: ProcessRuntimeIdentity,
        stopped: bool,
        stop_signal: Option<i32>,
    ) -> Result<(), ProcessRuntimeEndpointError>;
}

/// Narrow capability returned to an executor for reporting its one terminal
/// result. It carries no process-table or control authority and is bound to the
/// VM generation and PID allocated for that execution.
#[derive(Clone)]
pub struct ProcessExitReporter {
    identity: ProcessRuntimeIdentity,
    sink: Arc<dyn ProcessExitSink>,
}

impl fmt::Debug for ProcessExitReporter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessExitReporter")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl ProcessExitReporter {
    pub(crate) fn new(identity: ProcessRuntimeIdentity, sink: Arc<dyn ProcessExitSink>) -> Self {
        Self { identity, sink }
    }

    pub fn identity(&self) -> ProcessRuntimeIdentity {
        self.identity
    }

    pub fn report_exit(&self, termination: ProcessExit) -> Result<(), ProcessRuntimeEndpointError> {
        self.sink.report_exit(self.identity, termination)
    }

    pub fn report_runtime_fault(
        &self,
        fault: ProcessRuntimeFault,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        self.sink.report_runtime_fault(self.identity, fault)
    }
}

pub trait ProcessRuntimeEndpoint: Send + Sync {
    fn identity(&self) -> Option<ProcessRuntimeIdentity>;

    /// Whether a backend receiver is attached and able to make progress. The
    /// kernel uses this only to avoid teardown grace waits for deliberately
    /// virtual or never-started processes.
    fn has_control_consumer(&self) -> bool {
        true
    }

    fn request_control(
        &self,
        request: ProcessControlRequest,
    ) -> Result<(), ProcessRuntimeEndpointError>;
}

/// Coalesced controls observed by an execution at one safe point.
///
/// Stop/continue is last-writer-wins. Terminal controls are never stored in an
/// ordinary bounded event queue and therefore cannot be rejected by output or
/// host-call backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProcessControlBatch {
    pub checkpoint: bool,
    pub stopped: Option<bool>,
    pub stop_signal: Option<i32>,
    pub termination: Option<ProcessTermination>,
    pub cancellation: Option<ProcessCancellationReason>,
    checkpoint_revision: u64,
    stopped_revision: u64,
    termination_revision: u64,
    cancellation_revision: u64,
}

impl ProcessControlBatch {
    pub fn is_empty(self) -> bool {
        !self.checkpoint
            && self.stopped.is_none()
            && self.termination.is_none()
            && self.cancellation.is_none()
    }
}

pub type ProcessControlWake = Arc<dyn Fn() + Send + Sync + 'static>;

/// Two-part endpoint used while process allocation and backend construction
/// depend on one another.
///
/// The producer is registered with the kernel before a backend exists. The
/// sidecar binds the allocated PID and attaches exactly one receiver before it
/// starts guest instructions. Controls requested in between remain durable.
#[derive(Clone)]
pub struct RuntimeControlCell {
    inner: Arc<RuntimeControlCellInner>,
}

struct RuntimeControlCellInner {
    generation: u64,
    ack_sink: Option<Arc<dyn ProcessControlAckSink>>,
    state: Mutex<RuntimeControlState>,
}

#[derive(Default)]
struct RuntimeControlState {
    pid: Option<u32>,
    receiver_attached: bool,
    wake: Option<ProcessControlWake>,
    wake_pending: bool,
    checkpoint: bool,
    checkpoint_revision: u64,
    stopped: Option<bool>,
    stop_signal: Option<i32>,
    stopped_revision: u64,
    termination: Option<ProcessTermination>,
    termination_revision: u64,
    cancellation: Option<ProcessCancellationReason>,
    cancellation_revision: u64,
    next_revision: u64,
}

impl fmt::Debug for RuntimeControlCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeControlCell")
            .field("identity", &self.identity())
            .finish_non_exhaustive()
    }
}

impl RuntimeControlCell {
    pub fn new(generation: u64) -> Self {
        Self::with_ack_sink(generation, None)
    }

    pub(crate) fn new_with_ack_sink(
        generation: u64,
        ack_sink: Arc<dyn ProcessControlAckSink>,
    ) -> Self {
        Self::with_ack_sink(generation, Some(ack_sink))
    }

    fn with_ack_sink(generation: u64, ack_sink: Option<Arc<dyn ProcessControlAckSink>>) -> Self {
        Self {
            inner: Arc::new(RuntimeControlCellInner {
                generation,
                ack_sink,
                state: Mutex::new(RuntimeControlState::default()),
            }),
        }
    }

    /// Binds the PID allocated by the kernel. Binding is idempotent only for
    /// the same PID; rebinding would let a stale capability target a new
    /// process and is rejected.
    pub fn bind_pid(&self, pid: u32) -> Result<(), ProcessRuntimeEndpointError> {
        let mut state = lock_or_recover(&self.inner.state);
        match state.pid {
            None => {
                state.pid = Some(pid);
                Ok(())
            }
            Some(bound) if bound == pid => Ok(()),
            Some(bound) => Err(ProcessRuntimeEndpointError::new(
                "ESTALE",
                format!("runtime endpoint is bound to pid {bound}, not pid {pid}"),
            )),
        }
    }

    /// Attaches the backend-side consumer. If control was requested during
    /// construction, the supplied wake is called after the lock is released.
    pub fn attach(
        &self,
        wake: ProcessControlWake,
    ) -> Result<RuntimeControlReceiver, ProcessRuntimeEndpointError> {
        let should_wake = {
            let mut state = lock_or_recover(&self.inner.state);
            if state.pid.is_none() {
                return Err(ProcessRuntimeEndpointError::new(
                    "EINVAL",
                    "runtime endpoint must be bound to a kernel pid before attachment",
                ));
            }
            if state.receiver_attached {
                return Err(ProcessRuntimeEndpointError::new(
                    "EALREADY",
                    "runtime endpoint already has a control receiver",
                ));
            }
            state.receiver_attached = true;
            state.wake = Some(Arc::clone(&wake));
            state.wake_pending
        };
        if should_wake {
            wake();
        }
        Ok(RuntimeControlReceiver {
            inner: Arc::clone(&self.inner),
        })
    }
}

impl ProcessRuntimeEndpoint for RuntimeControlCell {
    fn identity(&self) -> Option<ProcessRuntimeIdentity> {
        lock_or_recover(&self.inner.state)
            .pid
            .map(|pid| ProcessRuntimeIdentity {
                generation: self.inner.generation,
                pid,
            })
    }

    fn has_control_consumer(&self) -> bool {
        lock_or_recover(&self.inner.state).receiver_attached
    }

    fn request_control(
        &self,
        request: ProcessControlRequest,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        let wake = {
            let mut state = lock_or_recover(&self.inner.state);
            if state.pid.is_none() {
                return Err(ProcessRuntimeEndpointError::new(
                    "EINVAL",
                    "runtime endpoint is not bound to a kernel pid",
                ));
            }
            match request {
                ProcessControlRequest::Checkpoint => {
                    state.checkpoint = true;
                    state.checkpoint_revision = take_next_revision(&mut state);
                }
                ProcessControlRequest::Stop { signal } => {
                    state.stopped = Some(true);
                    state.stop_signal = Some(signal);
                    state.stopped_revision = take_next_revision(&mut state);
                }
                ProcessControlRequest::Continue => {
                    state.stopped = Some(false);
                    state.stop_signal = None;
                    state.stopped_revision = take_next_revision(&mut state);
                }
                ProcessControlRequest::Terminate(termination) => {
                    state.termination = Some(prefer_termination(state.termination, termination));
                    state.termination_revision = take_next_revision(&mut state);
                }
                ProcessControlRequest::Cancel(reason) => {
                    if state.cancellation.is_none() {
                        state.cancellation = Some(reason);
                        state.cancellation_revision = take_next_revision(&mut state);
                    }
                }
            }
            if state.wake_pending {
                None
            } else {
                state.wake_pending = true;
                state.wake.clone()
            }
        };
        if let Some(wake) = wake {
            wake();
        }
        Ok(())
    }
}

fn take_next_revision(state: &mut RuntimeControlState) -> u64 {
    state.next_revision = state.next_revision.wrapping_add(1).max(1);
    state.next_revision
}

fn prefer_termination(
    current: Option<ProcessTermination>,
    requested: ProcessTermination,
) -> ProcessTermination {
    match (current, requested) {
        (
            Some(ProcessTermination::Signal {
                signal,
                force: true,
            }),
            _,
        ) => ProcessTermination::Signal {
            signal,
            force: true,
        },
        (_, forced @ ProcessTermination::Signal { force: true, .. }) => forced,
        (Some(current), _) => current,
        (None, requested) => requested,
    }
}

pub struct RuntimeControlReceiver {
    inner: Arc<RuntimeControlCellInner>,
}

impl fmt::Debug for RuntimeControlReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeControlReceiver")
            .field(
                "identity",
                &lock_or_recover(&self.inner.state)
                    .pid
                    .map(|pid| ProcessRuntimeIdentity {
                        generation: self.inner.generation,
                        pid,
                    }),
            )
            .finish_non_exhaustive()
    }
}

impl RuntimeControlReceiver {
    pub fn identity(&self) -> ProcessRuntimeIdentity {
        let state = lock_or_recover(&self.inner.state);
        ProcessRuntimeIdentity {
            generation: self.inner.generation,
            pid: state
                .pid
                .expect("a control receiver is only created after PID binding"),
        }
    }

    /// Replaces the coalesced wake target when lifecycle ownership moves to a
    /// shared process pump. Pending control is replayed to the new target.
    pub fn set_wake(&self, wake: ProcessControlWake) {
        let should_wake = {
            let mut state = lock_or_recover(&self.inner.state);
            state.wake = Some(Arc::clone(&wake));
            state.wake_pending
        };
        if should_wake {
            wake();
        }
    }

    /// Leases the complete current control snapshot without clearing it.
    /// Call [`Self::acknowledge`] only after every adapter action succeeds.
    pub fn pending(&self) -> ProcessControlBatch {
        let state = lock_or_recover(&self.inner.state);
        ProcessControlBatch {
            checkpoint: state.checkpoint,
            stopped: state.stopped,
            stop_signal: state.stop_signal,
            termination: state.termination,
            cancellation: state.cancellation,
            checkpoint_revision: state.checkpoint_revision,
            stopped_revision: state.stopped_revision,
            termination_revision: state.termination_revision,
            cancellation_revision: state.cancellation_revision,
        }
    }

    /// Acknowledges only the leased revisions. Controls requested concurrently
    /// remain durable and schedule another coalesced wake.
    pub fn acknowledge(
        &self,
        batch: ProcessControlBatch,
    ) -> Result<(), ProcessRuntimeEndpointError> {
        // The acknowledgement sink may lock the process table, which validates
        // this endpoint's identity by locking the cell again. Never invoke it
        // while holding the cell mutex.
        if let (Some(sink), Some(stopped)) = (&self.inner.ack_sink, batch.stopped) {
            sink.acknowledge_stop_state(self.identity(), stopped, batch.stop_signal)?;
        }

        let wake = {
            let mut state = lock_or_recover(&self.inner.state);
            if state.stopped_revision == batch.stopped_revision && state.stopped == batch.stopped {
                state.stopped = None;
                state.stop_signal = None;
            }
            if state.checkpoint_revision == batch.checkpoint_revision {
                state.checkpoint = false;
            }
            if state.termination_revision == batch.termination_revision
                && state.termination == batch.termination
            {
                state.termination = None;
            }
            if state.cancellation_revision == batch.cancellation_revision
                && state.cancellation == batch.cancellation
            {
                state.cancellation = None;
            }
            state.wake_pending = state.checkpoint
                || state.stopped.is_some()
                || state.termination.is_some()
                || state.cancellation.is_some();
            state.wake_pending.then(|| state.wake.clone()).flatten()
        };
        if let Some(wake) = wake {
            wake();
        }
        Ok(())
    }

    /// Replays the coalesced wake after a failed adapter action. The leased
    /// controls remain unchanged.
    pub fn retry_pending(&self) {
        let wake = {
            let mut state = lock_or_recover(&self.inner.state);
            let has_pending = state.checkpoint
                || state.stopped.is_some()
                || state.termination.is_some()
                || state.cancellation.is_some();
            state.wake_pending = has_pending;
            has_pending.then(|| state.wake.clone()).flatten()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}

impl Drop for RuntimeControlReceiver {
    fn drop(&mut self) {
        let mut state = lock_or_recover(&self.inner.state);
        state.wake = None;
        state.receiver_attached = false;
    }
}

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        eprintln!("ERR_AGENTOS_PROCESS_RUNTIME_CONTROL_POISONED: recovering runtime-control state");
        poisoned.into_inner()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingAckSink {
        transitions: Mutex<Vec<(ProcessRuntimeIdentity, bool, Option<i32>)>>,
    }

    impl ProcessControlAckSink for RecordingAckSink {
        fn acknowledge_stop_state(
            &self,
            identity: ProcessRuntimeIdentity,
            stopped: bool,
            stop_signal: Option<i32>,
        ) -> Result<(), ProcessRuntimeEndpointError> {
            self.transitions
                .lock()
                .expect("ack transitions lock poisoned")
                .push((identity, stopped, stop_signal));
            Ok(())
        }
    }

    #[test]
    fn runtime_fault_payloads_are_bounded_before_reporting() {
        let fault = ProcessRuntimeFault::try_new(
            "ERR_AGENTOS_WASM_TRAP",
            "unreachable",
            Some(serde_json::json!({ "trap": "unreachable" })),
        )
        .expect("bounded fault");
        assert_eq!(fault.code(), "ERR_AGENTOS_WASM_TRAP");
        assert_eq!(fault.message(), "unreachable");
        assert_eq!(fault.details().expect("details")["trap"], "unreachable");

        assert_eq!(
            ProcessRuntimeFault::try_new("", "missing code", None)
                .expect_err("empty code must fail")
                .code(),
            "EINVAL"
        );
        assert_eq!(
            ProcessRuntimeFault::try_new(
                "ERR_AGENTOS_WASM_TRAP",
                "x".repeat(MAX_RUNTIME_FAULT_MESSAGE_BYTES + 1),
                None,
            )
            .expect_err("oversized message must fail")
            .code(),
            "E2BIG"
        );
        assert_eq!(
            ProcessRuntimeFault::try_new(
                "ERR_AGENTOS_WASM_TRAP",
                "details",
                Some(serde_json::json!({
                    "payload": "x".repeat(MAX_RUNTIME_FAULT_DETAILS_BYTES)
                })),
            )
            .expect_err("oversized details must fail")
            .code(),
            "E2BIG"
        );
    }

    #[test]
    fn pre_attach_controls_are_durable_and_wake_once() {
        let cell = RuntimeControlCell::new(7);
        cell.bind_pid(41).expect("bind pid");
        cell.request_control(ProcessControlRequest::Checkpoint)
            .expect("checkpoint");
        cell.request_control(ProcessControlRequest::Stop { signal: 20 })
            .expect("stop");
        cell.request_control(ProcessControlRequest::Checkpoint)
            .expect("coalesced checkpoint");

        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = Arc::clone(&wakes);
        let receiver = cell
            .attach(Arc::new(move || {
                wake_count.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("attach receiver");

        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(
            receiver.identity(),
            ProcessRuntimeIdentity {
                generation: 7,
                pid: 41
            }
        );
        let controls = receiver.pending();
        assert!(controls.checkpoint);
        assert_eq!(controls.stopped, Some(true));
        assert_eq!(controls.stop_signal, Some(20));
        receiver
            .acknowledge(controls)
            .expect("acknowledge controls");
        assert!(receiver.pending().is_empty());
    }

    #[test]
    fn one_wake_covers_a_coalesced_batch_and_rearms_after_take() {
        let cell = RuntimeControlCell::new(9);
        cell.bind_pid(3).expect("bind pid");
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = Arc::clone(&wakes);
        let receiver = cell
            .attach(Arc::new(move || {
                wake_count.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("attach receiver");

        cell.request_control(ProcessControlRequest::Stop { signal: 20 })
            .expect("stop");
        cell.request_control(ProcessControlRequest::Continue)
            .expect("continue");
        cell.request_control(ProcessControlRequest::Cancel(
            ProcessCancellationReason::VmTeardown,
        ))
        .expect("cancel");
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let controls = receiver.pending();
        assert_eq!(controls.stopped, Some(false));
        receiver
            .acknowledge(controls)
            .expect("acknowledge controls");

        cell.request_control(ProcessControlRequest::Checkpoint)
            .expect("checkpoint");
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn forced_termination_cannot_be_downgraded_or_dropped() {
        let cell = RuntimeControlCell::new(1);
        cell.bind_pid(99).expect("bind pid");
        let receiver = cell.attach(Arc::new(|| {})).expect("attach receiver");
        cell.request_control(ProcessControlRequest::Terminate(
            ProcessTermination::Signal {
                signal: 15,
                force: false,
            },
        ))
        .expect("term");
        cell.request_control(ProcessControlRequest::Terminate(
            ProcessTermination::Signal {
                signal: 9,
                force: true,
            },
        ))
        .expect("kill");
        cell.request_control(ProcessControlRequest::Terminate(
            ProcessTermination::RuntimeFault,
        ))
        .expect("later fault");
        assert_eq!(
            receiver.pending().termination,
            Some(ProcessTermination::Signal {
                signal: 9,
                force: true,
            })
        );
    }

    #[test]
    fn identity_cannot_be_rebound_or_attached_twice() {
        let cell = RuntimeControlCell::new(11);
        cell.bind_pid(5).expect("bind pid");
        assert_eq!(
            cell.bind_pid(6).expect_err("reject rebind").code(),
            "ESTALE"
        );
        let receiver = cell.attach(Arc::new(|| {})).expect("attach receiver");
        assert_eq!(
            cell.attach(Arc::new(|| {}))
                .expect_err("reject second receiver")
                .code(),
            "EALREADY"
        );
        drop(receiver);
        cell.attach(Arc::new(|| {})).expect("reattach after drop");
    }

    #[test]
    fn failed_application_replays_wake_without_clearing_controls() {
        let cell = RuntimeControlCell::new(13);
        cell.bind_pid(8).expect("bind pid");
        let wakes = Arc::new(AtomicUsize::new(0));
        let wake_count = Arc::clone(&wakes);
        let receiver = cell
            .attach(Arc::new(move || {
                wake_count.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("attach receiver");

        cell.request_control(ProcessControlRequest::Terminate(
            ProcessTermination::RuntimeFault,
        ))
        .expect("request termination");
        let controls = receiver.pending();
        receiver.retry_pending();

        assert_eq!(wakes.load(Ordering::Relaxed), 2);
        assert_eq!(receiver.pending(), controls);
        receiver.acknowledge(controls).expect("acknowledge retry");
        assert!(receiver.pending().is_empty());
    }

    #[test]
    fn acknowledgement_clears_only_leased_revisions() {
        let sink = Arc::new(RecordingAckSink::default());
        let cell = RuntimeControlCell::new_with_ack_sink(17, sink.clone());
        cell.bind_pid(12).expect("bind pid");
        let receiver = cell.attach(Arc::new(|| {})).expect("attach receiver");

        cell.request_control(ProcessControlRequest::Stop { signal: 20 })
            .expect("request stop");
        let stopped = receiver.pending();
        cell.request_control(ProcessControlRequest::Continue)
            .expect("request continue concurrently");

        receiver
            .acknowledge(stopped)
            .expect("acknowledge applied stop");
        let continued = receiver.pending();
        assert_eq!(continued.stopped, Some(false));
        receiver
            .acknowledge(continued)
            .expect("acknowledge applied continue");
        assert!(receiver.pending().is_empty());
        assert_eq!(
            sink.transitions
                .lock()
                .expect("ack transitions lock poisoned")
                .as_slice(),
            &[
                (
                    ProcessRuntimeIdentity {
                        generation: 17,
                        pid: 12,
                    },
                    true,
                    Some(20),
                ),
                (
                    ProcessRuntimeIdentity {
                        generation: 17,
                        pid: 12,
                    },
                    false,
                    None,
                ),
            ]
        );
    }
}
