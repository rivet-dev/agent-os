//! Runtime-neutral host-service requests.
//!
//! These types contain owned values only. Guest pointers, V8 handles,
//! Wasmtime Stores, Python objects, and sidecar process borrows are adapter
//! concerns and must never enter this module.

mod clock;
mod entropy;
mod filesystem;
mod identity;
mod network;
mod process;
mod signal;
mod terminal;

pub use clock::*;
pub use entropy::*;
pub use filesystem::*;
pub use identity::*;
pub use network::*;
pub use process::*;
pub use signal::*;
pub use terminal::*;

use crate::backend::{
    DirectHostReplyHandle, ExecutionEvent, ExecutionEventAdmission, ExecutionEventSubmitHandle,
    HostServiceError, PayloadLimit,
};
use std::fmt;
use std::sync::Arc;
/// Authority identifying the already-registered process issuing a host call.
/// Permission tier and resource rights are looked up from kernel state; they
/// are intentionally not caller-selectable fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HostProcessContext {
    pub generation: u64,
    pub pid: u32,
}

/// Runtime-neutral operation accepted by the shared sidecar host dispatcher.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum HostOperation {
    Filesystem(FilesystemOperation),
    Network(NetworkOperation),
    Process(ProcessOperation),
    Terminal(TerminalOperation),
    Signal(SignalOperation),
    Identity(IdentityOperation),
    Clock(ClockOperation),
    Entropy(EntropyOperation),
}

/// Capability-sized submission interface. Implementations enqueue or execute
/// bounded work and settle only the supplied direct reply handle.
pub trait HostCapability<Operation>: Send + Sync {
    fn submit(
        &self,
        process: HostProcessContext,
        operation: Operation,
        reply: DirectHostReplyHandle,
        admission: ExecutionEventAdmission,
    ) -> Result<(), HostServiceError>;
}

/// Complete runtime-neutral host-service bundle supplied to an executor.
///
/// The bundle is deliberately a router over capability-sized interfaces, not
/// a mega-trait. Executors can therefore share the exact filesystem, network,
/// process, terminal, signal, identity, clock, and entropy implementations
/// without depending on a sidecar execution enum or another engine.
#[derive(Clone)]
pub struct HostCapabilitySet {
    filesystem: Arc<dyn HostCapability<FilesystemOperation>>,
    network: Arc<dyn HostCapability<NetworkOperation>>,
    process: Arc<dyn HostCapability<ProcessOperation>>,
    terminal: Arc<dyn HostCapability<TerminalOperation>>,
    signal: Arc<dyn HostCapability<SignalOperation>>,
    identity: Arc<dyn HostCapability<IdentityOperation>>,
    clock: Arc<dyn HostCapability<ClockOperation>>,
    entropy: Arc<dyn HostCapability<EntropyOperation>>,
}

impl fmt::Debug for HostCapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostCapabilitySet").finish_non_exhaustive()
    }
}

impl HostCapabilitySet {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        filesystem: Arc<dyn HostCapability<FilesystemOperation>>,
        network: Arc<dyn HostCapability<NetworkOperation>>,
        process: Arc<dyn HostCapability<ProcessOperation>>,
        terminal: Arc<dyn HostCapability<TerminalOperation>>,
        signal: Arc<dyn HostCapability<SignalOperation>>,
        identity: Arc<dyn HostCapability<IdentityOperation>>,
        clock: Arc<dyn HostCapability<ClockOperation>>,
        entropy: Arc<dyn HostCapability<EntropyOperation>>,
    ) -> Self {
        Self {
            filesystem,
            network,
            process,
            terminal,
            signal,
            identity,
            clock,
            entropy,
        }
    }

    pub fn submit(
        &self,
        process: HostProcessContext,
        operation: HostOperation,
        reply: DirectHostReplyHandle,
        admission: ExecutionEventAdmission,
    ) -> Result<(), HostServiceError> {
        match operation {
            HostOperation::Filesystem(operation) => {
                self.filesystem.submit(process, operation, reply, admission)
            }
            HostOperation::Network(operation) => {
                self.network.submit(process, operation, reply, admission)
            }
            HostOperation::Process(operation) => {
                self.process.submit(process, operation, reply, admission)
            }
            HostOperation::Terminal(operation) => {
                self.terminal.submit(process, operation, reply, admission)
            }
            HostOperation::Signal(operation) => {
                self.signal.submit(process, operation, reply, admission)
            }
            HostOperation::Identity(operation) => {
                self.identity.submit(process, operation, reply, admission)
            }
            HostOperation::Clock(operation) => {
                self.clock.submit(process, operation, reply, admission)
            }
            HostOperation::Entropy(operation) => {
                self.entropy.submit(process, operation, reply, admission)
            }
        }
    }

    /// Build capability-family adapters over one bounded common-event lane.
    /// The adapters only wrap typed requests; all filesystem, network,
    /// process, terminal, signal, identity, clock, and entropy semantics stay
    /// in their sidecar capability-family implementations.
    pub fn from_event_submission(events: ExecutionEventSubmitHandle) -> Self {
        let adapter = Arc::new(EventSubmittingCapability { events });
        Self::new(
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter.clone(),
            adapter,
        )
    }
}

/// Cloneable executor-facing host services bound to one kernel process
/// generation. Callers cannot select another PID or generation per request.
#[derive(Clone)]
pub struct ProcessHostCapabilitySet {
    process: HostProcessContext,
    capabilities: HostCapabilitySet,
    event_submission: Option<ExecutionEventSubmitHandle>,
}

impl fmt::Debug for ProcessHostCapabilitySet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessHostCapabilitySet")
            .field("process", &self.process)
            .finish_non_exhaustive()
    }
}

impl ProcessHostCapabilitySet {
    pub fn new(process: HostProcessContext, capabilities: HostCapabilitySet) -> Self {
        Self {
            process,
            capabilities,
            event_submission: None,
        }
    }

    pub fn from_event_submission(events: ExecutionEventSubmitHandle) -> Self {
        let process = events.process();
        Self {
            process,
            capabilities: HostCapabilitySet::from_event_submission(events.clone()),
            event_submission: Some(events),
        }
    }

    pub fn process(&self) -> HostProcessContext {
        self.process
    }

    pub fn admit_request(
        &self,
        retained_bytes: usize,
    ) -> Result<ExecutionEventAdmission, HostServiceError> {
        self.event_submission
            .as_ref()
            .ok_or_else(|| {
                HostServiceError::new(
                    "ENOTSUP",
                    "host capability set does not use a common-event submission lane",
                )
            })?
            .admit(retained_bytes)
    }

    pub fn admit_json_request<T: serde::Serialize + ?Sized>(
        &self,
        value: &T,
        additional_raw_bytes: usize,
    ) -> Result<ExecutionEventAdmission, HostServiceError> {
        self.event_submission
            .as_ref()
            .ok_or_else(|| {
                HostServiceError::new(
                    "ENOTSUP",
                    "host capability set does not use a common-event submission lane",
                )
            })?
            .admit_json(value, additional_raw_bytes)
    }

    pub fn submit(
        &self,
        operation: HostOperation,
        reply: DirectHostReplyHandle,
        admission: ExecutionEventAdmission,
    ) -> Result<(), HostServiceError> {
        self.capabilities
            .submit(self.process, operation, reply, admission)
    }
}

struct EventSubmittingCapability {
    events: ExecutionEventSubmitHandle,
}

macro_rules! impl_event_submitting_capability {
    ($operation:ty, $variant:path) => {
        impl HostCapability<$operation> for EventSubmittingCapability {
            fn submit(
                &self,
                process: HostProcessContext,
                operation: $operation,
                reply: DirectHostReplyHandle,
                admission: ExecutionEventAdmission,
            ) -> Result<(), HostServiceError> {
                if process != self.events.process() {
                    let error = HostServiceError::new(
                        "ESTALE",
                        "host capability used with a different process generation",
                    )
                    .with_details(serde_json::json!({
                        "expectedGeneration": self.events.process().generation,
                        "expectedPid": self.events.process().pid,
                        "actualGeneration": process.generation,
                        "actualPid": process.pid,
                    }));
                    reply.fail(error.clone())?;
                    return Err(error);
                }
                self.events.submit(
                    ExecutionEvent::HostCall {
                        operation: $variant(operation),
                        reply,
                    },
                    admission,
                )
            }
        }
    };
}

impl_event_submitting_capability!(FilesystemOperation, HostOperation::Filesystem);
impl_event_submitting_capability!(NetworkOperation, HostOperation::Network);
impl_event_submitting_capability!(ProcessOperation, HostOperation::Process);
impl_event_submitting_capability!(TerminalOperation, HostOperation::Terminal);
impl_event_submitting_capability!(SignalOperation, HostOperation::Signal);
impl_event_submitting_capability!(IdentityOperation, HostOperation::Identity);
impl_event_submitting_capability!(ClockOperation, HostOperation::Clock);
impl_event_submitting_capability!(EntropyOperation, HostOperation::Entropy);

/// Owned bytes admitted before a request is queued or copied again.
#[derive(Clone, PartialEq, Eq)]
pub struct BoundedBytes {
    bytes: Vec<u8>,
}

/// A guest-selected count admitted against a named limit before the operation
/// is constructed. Keeping the field private prevents adapters from attaching
/// an unchecked allocation or result size to a queued host request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedUsize(usize);

impl BoundedUsize {
    pub fn try_new(value: usize, limit: &PayloadLimit) -> Result<Self, HostServiceError> {
        limit.admit(value)?;
        Ok(Self(value))
    }

    pub fn get(self) -> usize {
        self.0
    }
}

/// A collection admitted against an element-count limit before queueing.
#[derive(Clone, PartialEq, Eq)]
pub struct BoundedVec<T>(Vec<T>);

impl<T> std::fmt::Debug for BoundedVec<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedVec")
            .field("len", &self.0.len())
            .finish()
    }
}

impl<T> BoundedVec<T> {
    pub fn try_new(values: Vec<T>, limit: &PayloadLimit) -> Result<Self, HostServiceError> {
        limit.admit(values.len())?;
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }
}

impl std::fmt::Debug for BoundedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedBytes")
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl BoundedBytes {
    pub fn try_new(bytes: Vec<u8>, limit: &PayloadLimit) -> Result<Self, HostServiceError> {
        limit.admit(bytes.len())?;
        Ok(Self { bytes })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

/// Owned UTF-8 string admitted against an explicit byte limit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedString(String);

impl BoundedString {
    pub fn try_new(value: String, limit: &PayloadLimit) -> Result<Self, HostServiceError> {
        if let Err(mut error) = limit.admit(value.len()) {
            error.code = String::from("ENAMETOOLONG");
            return Err(error);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{
        bounded_execution_event_channel, DirectHostReplyTarget, HostCallIdentity, HostCallReply,
    };
    use std::marker::PhantomData;
    use std::sync::Mutex;

    struct RecordingReplyTarget;

    struct RejectingReplyTarget;

    impl DirectHostReplyTarget for RejectingReplyTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _: u64,
            _: bool,
            _: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            Err(HostServiceError::new(
                "EIO",
                "reply target rejected settlement",
            ))
        }
    }

    impl DirectHostReplyTarget for RecordingReplyTarget {
        fn claim(&self, _: u64) -> Result<bool, HostServiceError> {
            Ok(true)
        }

        fn respond(
            &self,
            _: u64,
            _: bool,
            result: Result<HostCallReply, HostServiceError>,
        ) -> Result<(), HostServiceError> {
            result.map(|_| ())
        }
    }

    struct RecordingCapability<Operation> {
        family: &'static str,
        seen: Arc<Mutex<Vec<&'static str>>>,
        operation: PhantomData<fn(Operation)>,
    }

    impl<Operation> RecordingCapability<Operation> {
        fn new(family: &'static str, seen: Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                family,
                seen,
                operation: PhantomData,
            }
        }
    }

    impl<Operation: Send + 'static> HostCapability<Operation> for RecordingCapability<Operation> {
        fn submit(
            &self,
            _: HostProcessContext,
            _: Operation,
            reply: DirectHostReplyHandle,
            _: ExecutionEventAdmission,
        ) -> Result<(), HostServiceError> {
            self.seen.lock().expect("seen lock").push(self.family);
            reply.succeed(HostCallReply::Empty)
        }
    }

    fn reply(call_id: u64) -> DirectHostReplyHandle {
        DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 7,
                pid: 42,
                call_id,
            },
            Arc::new(RecordingReplyTarget),
            1024,
        )
        .expect("reply handle")
    }

    #[test]
    fn bounded_values_reject_before_admission() {
        let limit = |name: &'static str| PayloadLimit::new(name, 4).expect("named limit");
        let error = BoundedBytes::try_new(vec![0; 5], &limit("maxWriteBytes")).unwrap_err();
        assert_eq!(error.code, "E2BIG");
        let error =
            BoundedString::try_new(String::from("abcde"), &limit("maxPathBytes")).unwrap_err();
        assert_eq!(error.code, "ENAMETOOLONG");
        let error = BoundedUsize::try_new(5, &limit("maxPollFds")).unwrap_err();
        assert_eq!(error.details.unwrap()["limitName"], "maxPollFds");
        let error = BoundedVec::try_new(vec![1, 2, 3, 4, 5], &limit("maxGroups")).unwrap_err();
        assert_eq!(error.code, "E2BIG");
    }

    #[test]
    fn event_capability_propagates_stale_reply_settlement_failure() {
        let bound = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let (events, _receiver) = bounded_execution_event_channel(
            bound,
            1,
            PayloadLimit::new("limits.process.pendingEventBytes", 128).expect("byte limit"),
            Arc::new(|| {}),
        )
        .expect("queue");
        let admission = events.admit(1).expect("request admission");
        let capability = EventSubmittingCapability { events };
        let reply = DirectHostReplyHandle::new(
            HostCallIdentity {
                generation: 8,
                pid: 42,
                call_id: 1,
            },
            Arc::new(RejectingReplyTarget),
            1024,
        )
        .expect("reply");
        let error = <EventSubmittingCapability as HostCapability<ProcessOperation>>::submit(
            &capability,
            HostProcessContext {
                generation: 8,
                pid: 42,
            },
            ProcessOperation::GetPid,
            reply,
            admission,
        )
        .expect_err("reply settlement failure must propagate");
        assert_eq!(error.code, "EIO");
    }

    #[test]
    fn capability_set_routes_every_family_without_an_executor_switchboard() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let capabilities = HostCapabilitySet::new(
            Arc::new(RecordingCapability::new("filesystem", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("network", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("process", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("terminal", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("signal", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("identity", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("clock", Arc::clone(&seen))),
            Arc::new(RecordingCapability::new("entropy", Arc::clone(&seen))),
        );
        let process = HostProcessContext {
            generation: 7,
            pid: 42,
        };
        let result_limit = PayloadLimit::new("maxResultBytes", 4).expect("result limit");
        let bounded_count = || BoundedUsize::try_new(1, &result_limit).unwrap();
        let operations = [
            HostOperation::Filesystem(FilesystemOperation::Preopens),
            HostOperation::Network(NetworkOperation::LocalAddress { fd: 3 }),
            HostOperation::Process(ProcessOperation::GetPid),
            HostOperation::Terminal(TerminalOperation::IsTerminal { fd: 0 }),
            HostOperation::Signal(SignalOperation::Pending),
            HostOperation::Identity(IdentityOperation::Get),
            HostOperation::Clock(ClockOperation::Resolution {
                clock: GuestClockId::Monotonic,
            }),
            HostOperation::Entropy(EntropyOperation {
                length: bounded_count(),
            }),
        ];
        for (call_id, operation) in operations.into_iter().enumerate() {
            let admission =
                ExecutionEventAdmission::try_new(1, &result_limit).expect("request admission");
            capabilities
                .submit(process, operation, reply(call_id as u64 + 1), admission)
                .expect("route operation");
        }
        assert_eq!(
            *seen.lock().expect("seen lock"),
            [
                "filesystem",
                "network",
                "process",
                "terminal",
                "signal",
                "identity",
                "clock",
                "entropy",
            ]
        );
    }
}
