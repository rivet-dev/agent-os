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
use web_time::{Duration, Instant};

pub use vfs::posix::usage::{
    measure_filesystem_usage, FileSystemStats, FileSystemUsage, DEFAULT_MAX_FILESYSTEM_BYTES,
    DEFAULT_MAX_INODE_COUNT,
};

pub const DEFAULT_MAX_PROCESSES: usize = 256;
// Keep the Linux-visible default high enough for conventional applications
// that deliberately reserve sparse descriptor ranges (for example
// F_DUPFD/closefrom at fd 512), while retaining a fixed bounded table.
pub const DEFAULT_MAX_OPEN_FDS: usize = 1024;
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

const MAX_PREAD_BYTES_LIMIT: &str = "limits.resources.maxPreadBytes";
const MAX_FD_WRITE_BYTES_LIMIT: &str = "limits.resources.maxFdWriteBytes";
const MAX_PROCESS_ARGV_BYTES_LIMIT: &str = "limits.resources.maxProcessArgvBytes";
const MAX_PROCESS_ENV_BYTES_LIMIT: &str = "limits.resources.maxProcessEnvBytes";
const MAX_READDIR_ENTRIES_LIMIT: &str = "limits.resources.maxReaddirEntries";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceSnapshot {
    pub running_processes: usize,
    pub stopped_processes: usize,
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
    limit_name: Option<&'static str>,
    limit: Option<usize>,
    observed: Option<usize>,
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
            limit_name: None,
            limit: None,
            observed: None,
        }
    }

    fn file_table_full(message: impl Into<String>) -> Self {
        Self {
            code: "ENFILE",
            message: message.into(),
            limit_name: None,
            limit: None,
            observed: None,
        }
    }

    fn filesystem_full(message: impl Into<String>) -> Self {
        Self {
            code: "ENOSPC",
            message: message.into(),
            limit_name: None,
            limit: None,
            observed: None,
        }
    }

    fn out_of_memory(message: impl Into<String>) -> Self {
        Self {
            code: "ENOMEM",
            message: message.into(),
            limit_name: None,
            limit: None,
            observed: None,
        }
    }

    fn point_limit(
        code: &'static str,
        limit_name: &'static str,
        limit: usize,
        observed: usize,
        description: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: format!(
                "{}; limitName={limit_name} limit={limit} observed={observed}; raise {limit_name}",
                description.into()
            ),
            limit_name: Some(limit_name),
            limit: Some(limit),
            observed: Some(observed),
        }
    }

    pub fn limit_name(&self) -> Option<&'static str> {
        self.limit_name
    }

    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    pub fn observed(&self) -> Option<usize> {
        self.observed
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
    pread_bytes: Option<Arc<QueueGauge>>,
    fd_write_bytes: Option<Arc<QueueGauge>>,
    process_argv_bytes: Option<Arc<QueueGauge>>,
    process_env_bytes: Option<Arc<QueueGauge>>,
    readdir_entries: Option<Arc<QueueGauge>>,
    blocking_read_ms: Option<Arc<QueueGauge>>,
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
            pread_bytes: register_resource_gauge(
                TrackedLimit::VmPreadBytes,
                limits.max_pread_bytes,
            ),
            fd_write_bytes: register_resource_gauge(
                TrackedLimit::VmFdWriteBytes,
                limits.max_fd_write_bytes,
            ),
            process_argv_bytes: register_resource_gauge(
                TrackedLimit::VmProcessArgvBytes,
                limits.max_process_argv_bytes,
            ),
            process_env_bytes: register_resource_gauge(
                TrackedLimit::VmProcessEnvBytes,
                limits.max_process_env_bytes,
            ),
            readdir_entries: register_resource_gauge(
                TrackedLimit::VmReaddirEntries,
                limits.max_readdir_entries,
            ),
            blocking_read_ms: register_resource_gauge_u64(
                TrackedLimit::VmBlockingReadMs,
                limits.max_blocking_read_ms,
            ),
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

/// One kernel blocking wait governed by `maxBlockingReadMs`. Callers wait only
/// until the next edge returned by [`Self::wait_slice`], then retry readiness;
/// the 80% edge emits through the same structured limit-warning registry as
/// byte/count caps without restarting or duplicating the operation.
#[derive(Debug)]
pub struct BlockingReadDeadline {
    started: Instant,
    limit: Duration,
    warning_at: Duration,
    warning_emitted: bool,
    gauge: Arc<QueueGauge>,
}

impl BlockingReadDeadline {
    fn new(limit: Duration, gauge: Arc<QueueGauge>) -> Self {
        Self {
            started: Instant::now(),
            limit,
            warning_at: limit.saturating_mul(4) / 5,
            warning_emitted: false,
            gauge,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed().min(self.limit)
    }

    fn observe_warning_edge(&mut self) {
        let elapsed = self.elapsed();
        if !self.warning_emitted && elapsed >= self.warning_at {
            self.warning_emitted = true;
            self.gauge
                .observe_depth(usize::try_from(elapsed.as_millis()).unwrap_or(usize::MAX));
        }
    }

    pub fn wait_slice(&mut self) -> Option<Duration> {
        self.observe_warning_edge();
        let elapsed = self.elapsed();
        if elapsed >= self.limit {
            return None;
        }
        let next_edge = if self.warning_emitted {
            self.limit
        } else {
            self.warning_at
        };
        Some(next_edge.saturating_sub(elapsed))
    }

    pub fn expired(&mut self) -> bool {
        self.observe_warning_edge();
        self.elapsed() >= self.limit
    }
}

impl Drop for BlockingReadDeadline {
    fn drop(&mut self) {
        self.gauge.observe_depth(0);
    }
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
            gauge.observe_depth(tracked_processes(snapshot));
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

    pub fn blocking_read_deadline(&self) -> Option<BlockingReadDeadline> {
        let limit = self.limits.max_blocking_read_ms?;
        let gauge = Arc::clone(self.gauges.blocking_read_ms.as_ref()?);
        Some(BlockingReadDeadline::new(
            Duration::from_millis(limit),
            gauge,
        ))
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
        let stopped_processes = process_list
            .values()
            .filter(|process| process.status == ProcessStatus::Stopped)
            .count();
        let socket_snapshot = sockets.snapshot();

        let snapshot = ResourceSnapshot {
            running_processes,
            stopped_processes,
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
            if tracked_processes(snapshot) >= limit {
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
        let total = argv_payload_bytes(command, args);
        if let Some(gauge) = &self.gauges.process_argv_bytes {
            gauge.observe_depth(total);
        }
        if let Some(limit) = self.limits.max_process_argv_bytes {
            if total > limit {
                return Err(ResourceError::point_limit(
                    "EINVAL",
                    MAX_PROCESS_ARGV_BYTES_LIMIT,
                    limit,
                    total,
                    format!("process argv payload {total} bytes exceeds configured limit {limit}"),
                ));
            }
        }

        Ok(())
    }

    pub fn check_process_env_bytes(
        &self,
        inherited_env: &BTreeMap<String, String>,
        overrides: &BTreeMap<String, String>,
    ) -> Result<(), ResourceError> {
        let total = merged_env_payload_bytes(inherited_env, overrides);
        if let Some(gauge) = &self.gauges.process_env_bytes {
            gauge.observe_depth(total);
        }
        if let Some(limit) = self.limits.max_process_env_bytes {
            if total > limit {
                return Err(ResourceError::point_limit(
                    "EINVAL",
                    MAX_PROCESS_ENV_BYTES_LIMIT,
                    limit,
                    total,
                    format!(
                        "process environment payload {total} bytes exceeds configured limit {limit}"
                    ),
                ));
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
        if let Some(gauge) = &self.gauges.pread_bytes {
            gauge.observe_depth(length);
        }
        if let Some(limit) = self.limits.max_pread_bytes {
            if length > limit {
                return Err(ResourceError::point_limit(
                    "EINVAL",
                    MAX_PREAD_BYTES_LIMIT,
                    limit,
                    length,
                    format!("pread length {length} exceeds configured limit {limit}"),
                ));
            }
        }

        Ok(())
    }

    pub fn check_fd_write_size(&self, size: usize) -> Result<(), ResourceError> {
        if let Some(gauge) = &self.gauges.fd_write_bytes {
            gauge.observe_depth(size);
        }
        if let Some(limit) = self.limits.max_fd_write_bytes {
            if size > limit {
                return Err(ResourceError::point_limit(
                    "EINVAL",
                    MAX_FD_WRITE_BYTES_LIMIT,
                    limit,
                    size,
                    format!("write size {size} exceeds configured limit {limit}"),
                ));
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
        if let Some(gauge) = &self.gauges.readdir_entries {
            gauge.observe_depth(entries);
        }
        if let Some(limit) = self.limits.max_readdir_entries {
            if entries > limit {
                return Err(ResourceError::point_limit(
                    "ENOMEM",
                    MAX_READDIR_ENTRIES_LIMIT,
                    limit,
                    entries,
                    format!(
                        "directory listing with {entries} entries exceeds configured limit {limit}"
                    ),
                ));
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

fn tracked_processes(snapshot: &ResourceSnapshot) -> usize {
    snapshot
        .running_processes
        .saturating_add(snapshot.stopped_processes)
        .saturating_add(snapshot.exited_processes)
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
            if matches!(
                warning.name,
                TrackedLimit::VmOpenFds
                    | TrackedLimit::VmPreadBytes
                    | TrackedLimit::VmFdWriteBytes
                    | TrackedLimit::VmProcessArgvBytes
                    | TrackedLimit::VmProcessEnvBytes
                    | TrackedLimit::VmReaddirEntries
            ) {
                sink.lock().expect("sink mutex").push(warning.clone());
            }
        }));

        let limits = ResourceLimits {
            max_open_fds: Some(10),
            max_pread_bytes: Some(100),
            max_fd_write_bytes: Some(100),
            max_process_argv_bytes: Some(100),
            max_process_env_bytes: Some(100),
            max_readdir_entries: Some(100),
            ..ResourceLimits::default()
        };
        let accountant = ResourceAccountant::new(limits);
        let snapshot = ResourceSnapshot {
            open_fds: 9, // 90% of the cap
            ..ResourceSnapshot::default()
        };
        accountant.observe_resource_gauges(&snapshot);

        // Point-in-time limits use the same central 80% warning mechanism.
        // Exercise each at its exact accepted boundary before checking +1.
        accountant
            .check_process_argv_bytes(&"a".repeat(99), &[])
            .expect("exact argv byte limit");
        accountant
            .check_process_env_bytes(
                &BTreeMap::new(),
                &BTreeMap::from([(String::new(), "e".repeat(98))]),
            )
            .expect("exact environment byte limit");
        accountant
            .check_pread_length(100)
            .expect("exact pread byte limit");
        accountant
            .check_fd_write_size(100)
            .expect("exact write byte limit");
        accountant
            .check_readdir_entries(100)
            .expect("exact readdir entry limit");

        let errors = [
            accountant
                .check_process_argv_bytes(&"a".repeat(100), &[])
                .expect_err("argv limit +1"),
            accountant
                .check_process_env_bytes(
                    &BTreeMap::new(),
                    &BTreeMap::from([(String::new(), "e".repeat(99))]),
                )
                .expect_err("environment limit +1"),
            accountant
                .check_pread_length(101)
                .expect_err("pread limit +1"),
            accountant
                .check_fd_write_size(101)
                .expect_err("write limit +1"),
            accountant
                .check_readdir_entries(101)
                .expect_err("readdir limit +1"),
        ];
        for (error, (code, name)) in errors.iter().zip([
            ("EINVAL", MAX_PROCESS_ARGV_BYTES_LIMIT),
            ("EINVAL", MAX_PROCESS_ENV_BYTES_LIMIT),
            ("EINVAL", MAX_PREAD_BYTES_LIMIT),
            ("EINVAL", MAX_FD_WRITE_BYTES_LIMIT),
            ("ENOMEM", MAX_READDIR_ENTRIES_LIMIT),
        ]) {
            assert_eq!(error.code(), code);
            assert_eq!(error.limit_name(), Some(name));
            assert_eq!(error.limit(), Some(100));
            assert_eq!(error.observed(), Some(101));
            assert!(error.to_string().contains("raise limits.resources."));
        }

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
        let warning_names = captured
            .lock()
            .unwrap()
            .iter()
            .map(|warning| warning.name)
            .collect::<std::collections::HashSet<_>>();
        for expected in [
            TrackedLimit::VmPreadBytes,
            TrackedLimit::VmFdWriteBytes,
            TrackedLimit::VmProcessArgvBytes,
            TrackedLimit::VmProcessEnvBytes,
            TrackedLimit::VmReaddirEntries,
        ] {
            assert!(
                warning_names.contains(&expected),
                "{expected:?} must warn at the exact configured boundary"
            );
        }
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
