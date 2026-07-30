use super::{
    BoundedBytes, BoundedString, BoundedUsize, BoundedVec, FilesystemOperation, SignalSetValue,
};
use crate::backend::{HostServiceError, PayloadLimit};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceLimitKind {
    AddressSpace,
    Core,
    Cpu,
    Data,
    FileSize,
    LockedMemory,
    OpenFiles,
    Processes,
    ResidentSet,
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimitValue {
    pub soft: Option<u64>,
    pub hard: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTarget {
    Any,
    Pid(u32),
    ProcessGroup(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DescriptorAction {
    Close(u32),
    Dup2 {
        from: u32,
        to: u32,
    },
    Open {
        target_fd: u32,
        operation: FilesystemOperation,
    },
    SetCloseOnExec {
        fd: u32,
        enabled: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessImage {
    pub executable: BoundedString,
    pub argv: BoundedVec<BoundedString>,
    pub env: BoundedVec<(BoundedString, BoundedString)>,
    pub cwd: BoundedString,
    pub descriptor_actions: BoundedVec<DescriptorAction>,
    pub process_group: Option<u32>,
    pub session_leader: bool,
}

/// Bounded view of the userspace image currently committed in the kernel.
/// Environment entries remain ordered key/value pairs so executor adapters
/// can encode the exact `key=value\0` Preview1 byte sequence without reading
/// or reconstructing process-local environment state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedProcessImage {
    pub argv: BoundedVec<BoundedString>,
    pub env: BoundedVec<(BoundedString, BoundedString)>,
}

/// One POSIX spawn file action decoded at an executor boundary.
///
/// The numeric command is retained because the AgentOS libc extension is the
/// versioned ABI authority for the action set. The sidecar validates the
/// command and every descriptor again before mutating kernel state.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProcessSpawnFileAction {
    pub command: u32,
    #[serde(rename = "guestFd", default)]
    pub guest_fd: Option<i32>,
    pub fd: i32,
    #[serde(rename = "sourceFd")]
    pub source_fd: i32,
    #[serde(rename = "guestSourceFd", default)]
    pub guest_source_fd: Option<i32>,
    pub oflag: i32,
    pub mode: u32,
    pub path: String,
    #[serde(rename = "closeFromGuestFds", default)]
    pub close_from_guest_fds: Vec<u32>,
}

/// Sidecar-owned network description inherited by a spawned process.
/// Resource ownership is resolved from the parent process; none of these
/// guest-provided identifiers grant authority by themselves.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSpawnHostNetworkDescriptor {
    pub guest_fd: u32,
    #[serde(default)]
    pub description_id: Option<String>,
    #[serde(default)]
    pub close_on_exec: bool,
    #[serde(default)]
    pub socket_id: Option<String>,
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub udp_socket_id: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// Runtime-neutral process launch options shared by V8, Wasmtime, and Python
/// adapters. These fields describe Linux process semantics and sidecar
/// runtime selection; they do not contain engine handles or guest memory.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, Default)]
pub struct ProcessLaunchOptions {
    #[serde(default)]
    pub argv0: Option<String>,
    #[serde(rename = "cloexecFds", default)]
    pub cloexec_fds: Vec<u32>,
    #[serde(rename = "localReplacement", default)]
    pub local_replacement: bool,
    #[serde(rename = "executableFd", default)]
    pub executable_fd: Option<u32>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(rename = "internalBootstrapEnv", default)]
    pub internal_bootstrap_env: BTreeMap<String, String>,
    #[serde(rename = "spawnAttrFlags", default)]
    pub spawn_attr_flags: u32,
    #[serde(rename = "spawnExactPath", default)]
    pub spawn_exact_path: bool,
    #[serde(rename = "spawnSearchPath", default)]
    pub spawn_search_path: Option<String>,
    #[serde(rename = "spawnSchedPolicy", default)]
    pub spawn_sched_policy: Option<i32>,
    #[serde(rename = "spawnSchedPriority", default)]
    pub spawn_sched_priority: Option<i32>,
    #[serde(rename = "spawnPgroup", default)]
    pub spawn_pgroup: Option<i32>,
    #[serde(rename = "spawnSignalDefaults", default)]
    pub spawn_signal_defaults: Vec<u32>,
    #[serde(rename = "spawnSignalMask", default)]
    pub spawn_signal_mask: Vec<u32>,
    #[serde(rename = "spawnFileActions", default)]
    pub spawn_file_actions: Vec<ProcessSpawnFileAction>,
    #[serde(rename = "spawnFdMappings", default)]
    pub spawn_fd_mappings: Vec<[u32; 2]>,
    #[serde(rename = "spawnHostNetFds", default)]
    pub spawn_host_net_fds: Vec<ProcessSpawnHostNetworkDescriptor>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub detached: bool,
    #[serde(default)]
    pub stdio: Vec<String>,
    #[serde(default)]
    pub timeout: Option<u64>,
    #[serde(rename = "killSignal", default)]
    pub kill_signal: Option<String>,
}

/// Fully owned runtime-neutral process image model. Executor adapters must
/// convert it to [`BoundedProcessLaunchRequest`] before queueing it as a host
/// operation; sidecar-internal launch preparation may use the plain form only
/// after that admission proof has been consumed.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ProcessLaunchRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub options: ProcessLaunchOptions,
}

/// A process launch admitted against the adapter's configured request-byte
/// limit before it can become a queued [`ProcessOperation`]. Keeping the inner
/// request private prevents a new executor from bypassing payload admission.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundedProcessLaunchRequest(ProcessLaunchRequest);

impl BoundedProcessLaunchRequest {
    pub fn try_new(
        request: ProcessLaunchRequest,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        limit.admit_json(&request)?;
        Ok(Self(request))
    }

    pub fn as_request(&self) -> &ProcessLaunchRequest {
        &self.0
    }

    pub fn into_request(self) -> ProcessLaunchRequest {
        self.0
    }
}

/// Exact source selected for an executable-image snapshot.
///
/// Descriptor loading is intentionally a kernel operation: it reads the open
/// file description without advancing its cursor, so V8 and Wasmtime cannot
/// diverge through separate fd projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutableImageSource {
    /// Client-selected initial image admitted by the trusted sidecar before
    /// guest execution starts. This authority is never exposed as a guest
    /// import and does not apply to spawn/exec images.
    TrustedInitialPath(BoundedString),
    Path(BoundedString),
    Descriptor(u32),
}

/// Linux process-image context required when an executable snapshot may be a
/// shebang script. This is admitted before it can enter the host-operation
/// queue; the kernel owns interpreter resolution and argv rewriting.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableImageResolutionRequest {
    pub argv: Vec<String>,
    #[serde(default)]
    pub close_on_exec_fds: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedExecutableImageResolutionRequest(ExecutableImageResolutionRequest);

impl BoundedExecutableImageResolutionRequest {
    pub fn try_new(
        request: ExecutableImageResolutionRequest,
        limit: &PayloadLimit,
    ) -> Result<Self, HostServiceError> {
        limit.admit_json(&request)?;
        Ok(Self(request))
    }

    pub fn as_request(&self) -> &ExecutableImageResolutionRequest {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum ProcessOperation {
    Spawn(BoundedProcessLaunchRequest),
    /// Spawn a child, capture its bounded stdout/stderr, and settle only when
    /// the child exits. This is the common operation behind synchronous
    /// language-runtime subprocess helpers.
    RunCaptured {
        request: BoundedProcessLaunchRequest,
        max_buffer: BoundedUsize,
    },
    /// Replace the current process image. `options.executable_fd` selects the
    /// prepared in-place fexecve commit used after an executor has loaded the
    /// exact open-file image; absence selects ordinary pathname execve.
    Exec(BoundedProcessLaunchRequest),
    /// Authorize and retain one immutable executable-image snapshot outside
    /// the guest descriptor table. The sidecar bounds the snapshot with the
    /// VM's WASM module-file limit and returns an opaque generation handle.
    OpenExecutableImage {
        source: ExecutableImageSource,
        /// Present for exec/fexec snapshots, absent for an already-resolved
        /// trusted initial module.
        resolution: Option<BoundedExecutableImageResolutionRequest>,
    },
    ReadExecutableImage {
        handle: u64,
        offset: u64,
        max_bytes: super::BoundedUsize,
    },
    CloseExecutableImage {
        handle: u64,
    },
    PollChild {
        child_id: BoundedString,
        wait_ms: u64,
    },
    WriteChildStdin {
        child_id: BoundedString,
        chunk: BoundedBytes,
    },
    CloseChildStdin {
        child_id: BoundedString,
    },
    Wait {
        target: WaitTarget,
        options: u32,
        deadline_ms: Option<u64>,
        temporary_mask: Option<SignalSetValue>,
    },
    /// Consume only a stopped/continued child transition. This is separate
    /// from `Wait` so an adapter cannot accidentally consume terminal status
    /// while it is coordinating the child's final output event.
    WaitTransition {
        target: WaitTarget,
        options: u32,
    },
    Kill {
        target: i32,
        signal: i32,
    },
    GetImage {
        max_reply_bytes: BoundedUsize,
    },
    GetPid,
    GetParentPid,
    GetProcessGroup {
        pid: Option<u32>,
    },
    SetProcessGroup {
        pid: Option<u32>,
        pgid: Option<u32>,
    },
    GetResourceLimit {
        kind: ResourceLimitKind,
    },
    SetResourceLimit {
        kind: ResourceLimitKind,
        value: ResourceLimitValue,
    },
    Umask {
        new_mask: Option<u32>,
    },
    SystemIdentity,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_process_launch_requires_named_payload_admission() {
        let request = ProcessLaunchRequest {
            command: format!("/{}", "x".repeat(128)),
            args: Vec::new(),
            options: ProcessLaunchOptions::default(),
        };
        let limit =
            PayloadLimit::new("limits.reactor.maxBridgeRequestBytes", 64).expect("launch limit");
        let error = BoundedProcessLaunchRequest::try_new(request, &limit)
            .expect_err("oversized launch must not become a host operation");
        assert_eq!(error.code, "E2BIG");
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details["limitName"].as_str()),
            Some("limits.reactor.maxBridgeRequestBytes")
        );
    }
}
