use super::wake::{ExecutionWakeHandle, ExecutionWakeIdentity};
use super::{ExecutionExit, HostServiceError};
use crate::host::ProcessHostCapabilitySet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionBackendKind {
    Javascript,
    Python,
    WebAssembly,
    Binding,
}

/// Determines who consumes the kernel's authoritative descendant wait state.
///
/// Language runtimes with a guest-visible POSIX process model must leave
/// zombies available for the guest's `waitpid`; runtimes whose child-process
/// API is implemented entirely by the sidecar can be reaped after delivering
/// their terminal event. The sidecar always consumes executor lifecycle events
/// and commits the exit to the kernel; this policy controls only who consumes
/// the resulting kernel wait state. It is an execution-model capability, not
/// an engine identity: every implementation of the WebAssembly language
/// backend uses the same guest-owned policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescendantWaitOwnership {
    Sidecar,
    Guest,
}

/// Determines who consumes inherited descendant stdout and stderr.
///
/// Sidecar-native child-process APIs create explicit stream objects and route
/// output to those objects. Language runtimes with a guest-visible POSIX
/// process model instead consume the inherited kernel descriptors themselves;
/// claiming those bytes for a sidecar bridge would make ordinary shell
/// redirection and nested commands silently lose output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DescendantOutputOwnership {
    SidecarBridge,
    GuestDescriptors,
}

/// How a synchronous compatibility transport submits a potentially blocking
/// descriptor write to the kernel.
///
/// This is an adapter capability, not an engine identity. A transport that
/// cannot suspend its synchronous dispatcher must probe with nonblocking
/// writes and retry after readiness; an async import can use the ordinary
/// kernel operation because its admitted guest task can yield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynchronousFdWritePolicy {
    Blocking,
    NonblockingRetry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Completed,
    Signal(i32),
    Deadline,
    VmTeardown,
    HostRequest,
    RuntimeFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownOutcome {
    AwaitExit,
    Exited(ExecutionExit),
    ForwardSignal { process_id: u32, signal: i32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalCheckpointOutcome {
    Published,
    ForwardToProcess { process_id: u32 },
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishedSignalCheckpoint {
    pub signal: i32,
    pub delivery_token: u64,
    pub flags: u32,
    pub thread_id: u32,
}

/// Sidecar-facing lifecycle shared by thread-affine and Send backends.
///
/// The owned backend is deliberately not required to be `Send`. Only its
/// generation-bound control, wake, and reply capabilities cross threads.
pub trait ExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind;

    fn synchronous_fd_write_policy(&self) -> SynchronousFdWritePolicy {
        SynchronousFdWritePolicy::Blocking
    }

    fn descendant_wait_ownership(&self) -> DescendantWaitOwnership {
        DescendantWaitOwnership::Sidecar
    }

    fn descendant_output_ownership(&self) -> DescendantOutputOwnership {
        DescendantOutputOwnership::SidecarBridge
    }

    /// Returns the host process that physically contains this execution, when
    /// the adapter runs out of process. Embedded backends return `None`.
    fn native_process_id(&self) -> Option<u32> {
        None
    }

    /// Returns the generation-bound, runtime-neutral wake capability for this
    /// execution. Backends without an asynchronous event lane use the default
    /// `None`; engine-specific session handles stay behind this boundary.
    fn wake_handle(&self, _identity: ExecutionWakeIdentity) -> Option<ExecutionWakeHandle> {
        None
    }

    /// Attach generation-bound common host services before a prepared backend
    /// starts. A native adapter retains this handle in its execution/Store
    /// state; compatibility adapters may continue decoding their legacy wire
    /// calls in the sidecar while using the same submitted event path.
    fn configure_host_services(&mut self, _host: ProcessHostCapabilitySet) {}

    fn is_prepared_for_start(&self) -> bool;

    fn start_prepared(&mut self) -> Result<(), HostServiceError>;

    fn begin_shutdown(
        &mut self,
        reason: ShutdownReason,
    ) -> Result<ShutdownOutcome, HostServiceError>;

    fn set_paused(&self, paused: bool) -> Result<(), HostServiceError>;

    fn write_stdin(&mut self, bytes: &[u8]) -> Result<(), HostServiceError>;

    fn close_stdin(&mut self) -> Result<(), HostServiceError>;

    fn deliver_signal_checkpoint(
        &self,
        identity: ExecutionWakeIdentity,
        signal: i32,
        delivery_token: u64,
        flags: u32,
        thread_id: u32,
    ) -> Result<SignalCheckpointOutcome, HostServiceError>;

    /// Takes one delivery already claimed by the kernel control plane and
    /// published into this adapter's bounded, generation-scoped inbox.
    fn take_signal_checkpoint(
        &self,
        _identity: ExecutionWakeIdentity,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        Ok(None)
    }

    fn take_signal_checkpoint_for_thread(
        &self,
        identity: ExecutionWakeIdentity,
        thread_id: u32,
    ) -> Result<Option<PublishedSignalCheckpoint>, HostServiceError> {
        if thread_id == 0 {
            self.take_signal_checkpoint(identity)
        } else {
            Ok(None)
        }
    }

    /// Drops checkpoints claimed by the replaced image after a successful
    /// kernel exec commit. The kernel has already cleared those delivery
    /// scopes, so the replacement must never report their stale tokens.
    fn discard_signal_checkpoints(
        &self,
        _identity: ExecutionWakeIdentity,
    ) -> Result<(), HostServiceError> {
        Ok(())
    }
}
