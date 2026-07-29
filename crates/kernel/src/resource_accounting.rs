use crate::fd_table::FdTableManager;
use crate::pipe_manager::PipeManager;
use crate::process_table::{ProcessStatus, ProcessTable};
use crate::pty::PtyManager;
use crate::socket_table::{SocketState, SocketTable};
use agentos_bridge::queue_tracker::{register_limit, QueueGauge, TrackedLimit};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use vfs::posix::usage::RootFilesystemResourceLimits;

pub use vfs::posix::usage::{
    measure_filesystem_usage, FileSystemStats, FileSystemUsage, DEFAULT_MAX_FILESYSTEM_BYTES,
    DEFAULT_MAX_INODE_COUNT,
};

pub const DEFAULT_MAX_PROCESSES: usize = 256;
pub const DEFAULT_MAX_OPEN_FDS: usize = 256;
pub const DEFAULT_MAX_PIPES: usize = 128;
pub const DEFAULT_MAX_PTYS: usize = 128;
pub const DEFAULT_MAX_SOCKETS: usize = 256;
pub const DEFAULT_MAX_CONNECTIONS: usize = 256;
pub const DEFAULT_MAX_SOCKET_BUFFERED_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_MAX_SOCKET_DATAGRAM_QUEUE_LEN: usize = 1_024;
pub const DEFAULT_BLOCKING_READ_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_MAX_PREAD_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_FD_WRITE_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_PROCESS_ARGV_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_PROCESS_ENV_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_READDIR_ENTRIES: usize = 4_096;
pub const DEFAULT_MAX_RECURSIVE_FS_DEPTH: usize = 128;
pub const DEFAULT_MAX_RECURSIVE_FS_ENTRIES: usize = 65_536;
pub const DEFAULT_VIRTUAL_CPU_COUNT: usize = 1;
pub const DEFAULT_MAX_WASM_MEMORY_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceSnapshot {
    pub running_processes: usize,
    pub exited_processes: usize,
    pub fd_tables: usize,
    pub open_fds: usize,
    pub pipes: usize,
    pub pipe_buffered_bytes: usize,
    pub ptys: usize,
    pub pty_buffered_input_bytes: usize,
    pub pty_buffered_output_bytes: usize,
    pub sockets: usize,
    pub socket_listeners: usize,
    pub socket_connections: usize,
    pub socket_buffered_bytes: usize,
    pub socket_datagram_queue_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub virtual_cpu_count: Option<usize>,
    pub max_processes: Option<usize>,
    pub max_open_fds: Option<usize>,
    pub max_pipes: Option<usize>,
    pub max_ptys: Option<usize>,
    pub max_sockets: Option<usize>,
    pub max_connections: Option<usize>,
    pub max_socket_buffered_bytes: Option<usize>,
    pub max_socket_datagram_queue_len: Option<usize>,
    pub max_filesystem_bytes: Option<u64>,
    pub max_inode_count: Option<usize>,
    pub max_blocking_read_ms: Option<u64>,
    pub max_pread_bytes: Option<usize>,
    pub max_fd_write_bytes: Option<usize>,
    pub max_process_argv_bytes: Option<usize>,
    pub max_process_env_bytes: Option<usize>,
    pub max_readdir_entries: Option<usize>,
    pub max_recursive_fs_depth: Option<usize>,
    pub max_recursive_fs_entries: Option<usize>,
    pub max_wasm_fuel: Option<u64>,
    pub max_wasm_memory_bytes: Option<u64>,
    pub max_wasm_stack_bytes: Option<usize>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            virtual_cpu_count: Some(DEFAULT_VIRTUAL_CPU_COUNT),
            max_processes: Some(DEFAULT_MAX_PROCESSES),
            max_open_fds: Some(DEFAULT_MAX_OPEN_FDS),
            max_pipes: Some(DEFAULT_MAX_PIPES),
            max_ptys: Some(DEFAULT_MAX_PTYS),
            max_sockets: Some(DEFAULT_MAX_SOCKETS),
            max_connections: Some(DEFAULT_MAX_CONNECTIONS),
            max_socket_buffered_bytes: Some(DEFAULT_MAX_SOCKET_BUFFERED_BYTES),
            max_socket_datagram_queue_len: Some(DEFAULT_MAX_SOCKET_DATAGRAM_QUEUE_LEN),
            max_filesystem_bytes: Some(DEFAULT_MAX_FILESYSTEM_BYTES),
            max_inode_count: Some(DEFAULT_MAX_INODE_COUNT),
            max_blocking_read_ms: Some(DEFAULT_BLOCKING_READ_TIMEOUT_MS),
            max_pread_bytes: Some(DEFAULT_MAX_PREAD_BYTES),
            max_fd_write_bytes: Some(DEFAULT_MAX_FD_WRITE_BYTES),
            max_process_argv_bytes: Some(DEFAULT_MAX_PROCESS_ARGV_BYTES),
            max_process_env_bytes: Some(DEFAULT_MAX_PROCESS_ENV_BYTES),
            max_readdir_entries: Some(DEFAULT_MAX_READDIR_ENTRIES),
            max_recursive_fs_depth: Some(DEFAULT_MAX_RECURSIVE_FS_DEPTH),
            max_recursive_fs_entries: Some(DEFAULT_MAX_RECURSIVE_FS_ENTRIES),
            max_wasm_fuel: None,
            // Match the Workers-style default memory envelope where sensible:
            // guests are bounded unless the trusted VM config raises the cap.
            max_wasm_memory_bytes: Some(DEFAULT_MAX_WASM_MEMORY_BYTES),
            max_wasm_stack_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceError {
    code: &'static str,
    message: String,
}

impl RootFilesystemResourceLimits for ResourceLimits {
    fn max_filesystem_bytes(&self) -> Option<u64> {
        self.max_filesystem_bytes
    }

    fn max_inode_count(&self) -> Option<usize> {
        self.max_inode_count
    }
}

impl ResourceError {
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn exhausted(message: impl Into<String>) -> Self {
        Self {
            code: "EAGAIN",
            message: message.into(),
        }
    }

    fn file_table_full(message: impl Into<String>) -> Self {
        Self {
            code: "ENFILE",
            message: message.into(),
        }
    }

    fn filesystem_full(message: impl Into<String>) -> Self {
        Self {
            code: "ENOSPC",
            message: message.into(),
        }
    }

    fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "EINVAL",
            message: message.into(),
        }
    }

    fn out_of_memory(message: impl Into<String>) -> Self {
        Self {
            code: "ENOMEM",
            message: message.into(),
        }
    }
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for ResourceError {}

#[derive(Debug, Clone, Default)]
/// Per-VM gauges for the saturating resource limits, registered with the central
/// limit registry so their usage is inspectable and they emit an edge-triggered
/// approach warning (~80%) before the guest hits the hard cap. Only limits that
/// are actually set get a gauge; unbounded (`None`) limits are skipped.
struct ResourceGauges {
    processes: Option<Arc<QueueGauge>>,
    open_fds: Option<Arc<QueueGauge>>,
    pipes: Option<Arc<QueueGauge>>,
    ptys: Option<Arc<QueueGauge>>,
    sockets: Option<Arc<QueueGauge>>,
    connections: Option<Arc<QueueGauge>>,
    socket_buffered_bytes: Option<Arc<QueueGauge>>,
    socket_datagram_queue_len: Option<Arc<QueueGauge>>,
    filesystem_bytes: Option<Arc<QueueGauge>>,
    inodes: Option<Arc<QueueGauge>>,
    recursive_fs_depth: Option<Arc<QueueGauge>>,
    recursive_fs_entries: Option<Arc<QueueGauge>>,
}

fn register_resource_gauge(name: TrackedLimit, limit: Option<usize>) -> Option<Arc<QueueGauge>> {
    limit.map(|capacity| register_limit(name, capacity))
}

fn register_resource_gauge_u64(name: TrackedLimit, limit: Option<u64>) -> Option<Arc<QueueGauge>> {
    limit.map(|capacity| register_limit(name, usize_saturating_from_u64(capacity)))
}

fn usize_saturating_from_u64(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

impl ResourceGauges {
    fn new(limits: &ResourceLimits) -> Self {
        Self {
            processes: register_resource_gauge(TrackedLimit::VmProcesses, limits.max_processes),
            open_fds: register_resource_gauge(TrackedLimit::VmOpenFds, limits.max_open_fds),
            pipes: register_resource_gauge(TrackedLimit::VmPipes, limits.max_pipes),
            ptys: register_resource_gauge(TrackedLimit::VmPtys, limits.max_ptys),
            sockets: register_resource_gauge(TrackedLimit::VmSockets, limits.max_sockets),
            connections: register_resource_gauge(
                TrackedLimit::VmConnections,
                limits.max_connections,
            ),
            socket_buffered_bytes: register_resource_gauge(
                TrackedLimit::VmSocketBufferedBytes,
                limits.max_socket_buffered_bytes,
            ),
            socket_datagram_queue_len: register_resource_gauge(
                TrackedLimit::VmSocketDatagramQueueLen,
                limits.max_socket_datagram_queue_len,
            ),
            filesystem_bytes: register_resource_gauge_u64(
                TrackedLimit::VmFilesystemBytes,
                limits.max_filesystem_bytes,
            ),
            inodes: register_resource_gauge(TrackedLimit::VmInodes, limits.max_inode_count),
            recursive_fs_depth: register_resource_gauge(
                TrackedLimit::VmRecursiveFsDepth,
                limits.max_recursive_fs_depth,
            ),
            recursive_fs_entries: register_resource_gauge(
                TrackedLimit::VmRecursiveFsEntries,
                limits.max_recursive_fs_entries,
            ),
        }
    }
}

pub struct ResourceAccountant {
    limits: ResourceLimits,
    gauges: ResourceGauges,
}

impl ResourceAccountant {
    pub fn new(limits: ResourceLimits) -> Self {
        let gauges = ResourceGauges::new(&limits);
        Self { limits, gauges }
    }

    /// Sample the saturating-resource gauges from a fresh snapshot so the central
    /// registry tracks usage and warns before any cap is reached.
    fn observe_resource_gauges(&self, snapshot: &ResourceSnapshot) {
        if let Some(gauge) = &self.gauges.processes {
            gauge.observe_depth(snapshot.running_processes + snapshot.exited_processes);
        }
        if let Some(gauge) = &self.gauges.open_fds {
            gauge.observe_depth(snapshot.open_fds);
        }
        if let Some(gauge) = &self.gauges.pipes {
            gauge.observe_depth(snapshot.pipes);
        }
        if let Some(gauge) = &self.gauges.ptys {
            gauge.observe_depth(snapshot.ptys);
        }
        if let Some(gauge) = &self.gauges.sockets {
            gauge.observe_depth(snapshot.sockets);
        }
        if let Some(gauge) = &self.gauges.connections {
            gauge.observe_depth(snapshot.socket_connections);
        }
        if let Some(gauge) = &self.gauges.socket_buffered_bytes {
            gauge.observe_depth(snapshot.socket_buffered_bytes);
        }
        if let Some(gauge) = &self.gauges.socket_datagram_queue_len {
            gauge.observe_depth(snapshot.socket_datagram_queue_len);
        }
    }

    pub fn limits(&self) -> &ResourceLimits {
        &self.limits
    }

    pub fn snapshot(
        &self,
        processes: &ProcessTable,
        fd_tables: &FdTableManager,
        pipes: &PipeManager,
        ptys: &PtyManager,
        sockets: &SocketTable,
    ) -> ResourceSnapshot {
        let process_list = processes.list_processes();
        let running_processes = process_list
            .values()
            .filter(|process| process.status == ProcessStatus::Running)
            .count();
        let exited_processes = process_list
            .values()
            .filter(|process| process.status == ProcessStatus::Exited)
            .count();
        let socket_snapshot = sockets.snapshot();

        let snapshot = ResourceSnapshot {
            running_processes,
            exited_processes,
            fd_tables: fd_tables.len(),
            open_fds: fd_tables.total_open_fds(),
            pipes: pipes.pipe_count(),
            pipe_buffered_bytes: pipes.buffered_bytes(),
            ptys: ptys.pty_count(),
            pty_buffered_input_bytes: ptys.buffered_input_bytes(),
            pty_buffered_output_bytes: ptys.buffered_output_bytes(),
            sockets: socket_snapshot.sockets,
            socket_listeners: socket_snapshot.listeners,
            socket_connections: socket_snapshot.connections,
            socket_buffered_bytes: socket_snapshot.buffered_bytes,
            socket_datagram_queue_len: socket_snapshot.datagram_queue_len,
        };
        self.observe_resource_gauges(&snapshot);
        snapshot
    }

    pub fn check_process_spawn(
        &self,
        snapshot: &ResourceSnapshot,
        additional_fds: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_processes {
            if snapshot.running_processes + snapshot.exited_processes >= limit {
                return Err(ResourceError::exhausted("maximum process limit reached"));
            }
        }

        self.check_open_fds(snapshot, additional_fds)
    }

    pub fn check_process_argv_bytes(
        &self,
        command: &str,
        args: &[String],
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_process_argv_bytes {
            let total = argv_payload_bytes(command, args);
            if total > limit {
                return Err(ResourceError::invalid_input(format!(
                    "process argv payload {total} bytes exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn check_process_env_bytes(
        &self,
        inherited_env: &BTreeMap<String, String>,
        overrides: &BTreeMap<String, String>,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_process_env_bytes {
            let total = merged_env_payload_bytes(inherited_env, overrides);
            if total > limit {
                return Err(ResourceError::invalid_input(format!(
                    "process environment payload {total} bytes exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn check_pipe_allocation(&self, snapshot: &ResourceSnapshot) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_pipes {
            if snapshot.pipes >= limit {
                return Err(ResourceError::exhausted("maximum pipe count reached"));
            }
        }

        self.check_open_fds(snapshot, 2)
    }

    pub fn check_pty_allocation(&self, snapshot: &ResourceSnapshot) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_ptys {
            if snapshot.ptys >= limit {
                return Err(ResourceError::exhausted("maximum PTY count reached"));
            }
        }

        self.check_open_fds(snapshot, 2)
    }

    pub fn check_socket_allocation(
        &self,
        snapshot: &ResourceSnapshot,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_sockets {
            if snapshot.sockets >= limit {
                return Err(ResourceError::exhausted("maximum socket count reached"));
            }
        }

        Ok(())
    }

    pub fn check_socket_state_transition(
        &self,
        snapshot: &ResourceSnapshot,
        current: SocketState,
        next: SocketState,
    ) -> Result<(), ResourceError> {
        if !current.counts_as_connection() && next.counts_as_connection() {
            if let Some(limit) = self.limits.max_connections {
                if snapshot.socket_connections >= limit {
                    return Err(ResourceError::exhausted("maximum connection count reached"));
                }
            }
        }

        Ok(())
    }

    pub fn check_socket_buffer_growth(
        &self,
        snapshot: &ResourceSnapshot,
        additional_bytes: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_socket_buffered_bytes {
            if snapshot
                .socket_buffered_bytes
                .saturating_add(additional_bytes)
                > limit
            {
                return Err(ResourceError::exhausted(
                    "maximum socket buffered byte limit reached",
                ));
            }
        }

        Ok(())
    }

    pub fn check_socket_datagram_enqueue(
        &self,
        snapshot: &ResourceSnapshot,
        additional_bytes: usize,
    ) -> Result<(), ResourceError> {
        self.check_socket_buffer_growth(snapshot, additional_bytes)?;
        if let Some(limit) = self.limits.max_socket_datagram_queue_len {
            if snapshot.socket_datagram_queue_len.saturating_add(1) > limit {
                return Err(ResourceError::exhausted(
                    "maximum socket datagram queue length reached",
                ));
            }
        }

        Ok(())
    }

    pub fn check_pread_length(&self, length: usize) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_pread_bytes {
            if length > limit {
                return Err(ResourceError::invalid_input(format!(
                    "pread length {length} exceeds limits.resources.maxPreadBytes {limit}; raise limits.resources.maxPreadBytes to permit a larger single read"
                )));
            }
        }

        Ok(())
    }

    pub fn check_fd_write_size(&self, size: usize) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_fd_write_bytes {
            if size > limit {
                return Err(ResourceError::invalid_input(format!(
                    "write size {size} exceeds limits.resources.maxFdWriteBytes {limit}; raise limits.resources.maxFdWriteBytes to permit a larger single write"
                )));
            }
        }

        Ok(())
    }

    pub fn check_fd_allocation(
        &self,
        snapshot: &ResourceSnapshot,
        additional_fds: usize,
    ) -> Result<(), ResourceError> {
        self.check_open_fds(snapshot, additional_fds)
    }

    pub fn max_readdir_entries(&self) -> Option<usize> {
        self.limits.max_readdir_entries
    }

    pub fn max_recursive_fs_depth(&self) -> Option<usize> {
        self.limits.max_recursive_fs_depth
    }

    pub fn max_recursive_fs_entries(&self) -> Option<usize> {
        self.limits.max_recursive_fs_entries
    }

    pub fn check_readdir_entries(&self, entries: usize) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_readdir_entries {
            if entries > limit {
                return Err(ResourceError::out_of_memory(format!(
                    "directory listing with {entries} entries exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn check_recursive_fs_depth(&self, depth: usize) -> Result<(), ResourceError> {
        if let Some(gauge) = self.gauges.recursive_fs_depth.as_ref() {
            gauge.observe_depth(depth);
        }
        if let Some(limit) = self.limits.max_recursive_fs_depth {
            if depth > limit {
                return Err(ResourceError::out_of_memory(format!(
                    "recursive filesystem operation depth {depth} exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn check_recursive_fs_entries(&self, entries: usize) -> Result<(), ResourceError> {
        if let Some(gauge) = self.gauges.recursive_fs_entries.as_ref() {
            gauge.observe_depth(entries);
        }
        if let Some(limit) = self.limits.max_recursive_fs_entries {
            if entries > limit {
                return Err(ResourceError::out_of_memory(format!(
                    "recursive filesystem operation with {entries} entries exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    fn check_open_fds(
        &self,
        snapshot: &ResourceSnapshot,
        additional_fds: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_open_fds {
            if snapshot.open_fds.saturating_add(additional_fds) > limit {
                return Err(ResourceError::file_table_full(
                    "maximum open file descriptor limit reached",
                ));
            }
        }

        Ok(())
    }

    pub fn check_filesystem_usage(
        &self,
        _usage: &FileSystemUsage,
        resulting_bytes: u64,
        resulting_inodes: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_filesystem_bytes {
            if resulting_bytes > limit {
                return Err(ResourceError::filesystem_full(
                    "maximum filesystem size limit reached; raise limits.resources.maxFilesystemBytes if needed",
                ));
            }
        }

        if let Some(limit) = self.limits.max_inode_count {
            if resulting_inodes > limit {
                return Err(ResourceError::filesystem_full(
                    "maximum inode count limit reached; raise limits.resources.maxInodeCount if needed",
                ));
            }
        }

        // Sample the gauges only on the success path: observing the *projected*
        // value before the bounds check would latch a spurious near/at-capacity
        // warning for a write that is then rejected and never actually applied.
        if let Some(gauge) = &self.gauges.filesystem_bytes {
            gauge.observe_depth(usize_saturating_from_u64(resulting_bytes));
        }
        if let Some(gauge) = &self.gauges.inodes {
            gauge.observe_depth(resulting_inodes);
        }
        Ok(())
    }
}

fn argv_payload_bytes(command: &str, args: &[String]) -> usize {
    let command_bytes = command.len().saturating_add(1);
    command_bytes.saturating_add(
        args.iter()
            .map(|arg| arg.len().saturating_add(1))
            .sum::<usize>(),
    )
}

fn env_entry_payload_bytes(key: &str, value: &str) -> usize {
    key.len()
        .saturating_add(1)
        .saturating_add(value.len())
        .saturating_add(1)
}

fn merged_env_payload_bytes(
    inherited_env: &BTreeMap<String, String>,
    overrides: &BTreeMap<String, String>,
) -> usize {
    let mut total = inherited_env
        .iter()
        .map(|(key, value)| env_entry_payload_bytes(key, value))
        .sum::<usize>();

    for (key, value) in overrides {
        if let Some(previous) = inherited_env.get(key) {
            total = total.saturating_sub(env_entry_payload_bytes(key, previous));
        }
        total = total.saturating_add(env_entry_payload_bytes(key, value));
    }

    total
}

#[cfg(test)]
mod gauge_tests {
    use super::*;
    use agentos_bridge::queue_tracker::{set_limit_warning_handler, LimitWarning, TrackedLimit};
    use std::sync::{Arc, Mutex};

    #[test]
    fn resource_gauges_track_usage_and_warn_on_approach() {
        let captured: Arc<Mutex<Vec<LimitWarning>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&captured);
        // Filter by name so a gauge from a concurrently-running test can't pollute.
        set_limit_warning_handler(Box::new(move |warning| {
            if warning.name == TrackedLimit::VmOpenFds {
                sink.lock().expect("sink mutex").push(warning.clone());
            }
        }));

        let limits = ResourceLimits {
            max_open_fds: Some(10),
            ..ResourceLimits::default()
        };
        let accountant = ResourceAccountant::new(limits);
        let snapshot = ResourceSnapshot {
            open_fds: 9, // 90% of the cap
            ..ResourceSnapshot::default()
        };
        accountant.observe_resource_gauges(&snapshot);

        // The gauge reflects the sampled usage...
        let gauge = accountant
            .gauges
            .open_fds
            .as_ref()
            .expect("open_fds gauge registered when the limit is set");
        assert_eq!(gauge.depth(), 9);
        assert_eq!(gauge.capacity(), 10);
        assert_eq!(gauge.high_water(), 9);

        // ...and crossing ~80% emits the approach warning to the host sink.
        assert!(
            captured
                .lock()
                .unwrap()
                .iter()
                .any(|warning| warning.name == TrackedLimit::VmOpenFds),
            "open_fds at 90% of cap must emit an approach warning"
        );
    }

    #[test]
    fn unset_limit_registers_no_gauge() {
        let limits = ResourceLimits {
            max_ptys: None,
            ..ResourceLimits::default()
        };
        let accountant = ResourceAccountant::new(limits);
        assert!(
            accountant.gauges.ptys.is_none(),
            "an unbounded (None) limit must not register a gauge"
        );
    }

    #[test]
    fn filesystem_gauge_not_latched_by_rejected_write() {
        let limits = ResourceLimits {
            max_filesystem_bytes: Some(1000),
            max_inode_count: Some(100),
            ..ResourceLimits::default()
        };
        let accountant = ResourceAccountant::new(limits);
        let usage = FileSystemUsage::default();

        // A write that would exceed the byte cap is rejected and must NOT latch
        // the gauge to the projected (never-applied) value.
        let rejected = accountant.check_filesystem_usage(&usage, 2000, 0);
        assert!(rejected.is_err());
        let bytes_gauge = accountant
            .gauges
            .filesystem_bytes
            .as_ref()
            .expect("filesystem_bytes gauge registered");
        assert_eq!(
            bytes_gauge.depth(),
            0,
            "a rejected over-limit write must not bump the gauge"
        );

        // A successful write does update it.
        accountant
            .check_filesystem_usage(&usage, 500, 7)
            .expect("under-limit write is accepted");
        assert_eq!(bytes_gauge.depth(), 500);
        assert_eq!(
            accountant.gauges.inodes.as_ref().unwrap().depth(),
            7,
            "inode gauge tracks the accepted value"
        );
    }
}
