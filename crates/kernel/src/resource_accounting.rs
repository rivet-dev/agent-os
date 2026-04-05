use crate::fd_table::FdTableManager;
use crate::pipe_manager::PipeManager;
use crate::process_table::{ProcessStatus, ProcessTable};
use crate::pty::PtyManager;
use crate::vfs::{VfsResult, VirtualFileSystem};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;

pub const DEFAULT_MAX_FILESYSTEM_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_INODE_COUNT: usize = 16_384;
pub const DEFAULT_BLOCKING_READ_TIMEOUT_MS: u64 = 5_000;
pub const DEFAULT_MAX_PREAD_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_FD_WRITE_BYTES: usize = 64 * 1024 * 1024;
pub const DEFAULT_MAX_PROCESS_ARGV_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_PROCESS_ENV_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_READDIR_ENTRIES: usize = 4_096;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_processes: Option<usize>,
    pub max_open_fds: Option<usize>,
    pub max_pipes: Option<usize>,
    pub max_ptys: Option<usize>,
    pub max_sockets: Option<usize>,
    pub max_connections: Option<usize>,
    pub max_filesystem_bytes: Option<u64>,
    pub max_inode_count: Option<usize>,
    pub max_blocking_read_ms: Option<u64>,
    pub max_pread_bytes: Option<usize>,
    pub max_fd_write_bytes: Option<usize>,
    pub max_process_argv_bytes: Option<usize>,
    pub max_process_env_bytes: Option<usize>,
    pub max_readdir_entries: Option<usize>,
    pub max_wasm_fuel: Option<u64>,
    pub max_wasm_memory_bytes: Option<u64>,
    pub max_wasm_stack_bytes: Option<usize>,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_processes: None,
            max_open_fds: None,
            max_pipes: None,
            max_ptys: None,
            max_sockets: None,
            max_connections: None,
            max_filesystem_bytes: Some(DEFAULT_MAX_FILESYSTEM_BYTES),
            max_inode_count: Some(DEFAULT_MAX_INODE_COUNT),
            max_blocking_read_ms: Some(DEFAULT_BLOCKING_READ_TIMEOUT_MS),
            max_pread_bytes: Some(DEFAULT_MAX_PREAD_BYTES),
            max_fd_write_bytes: Some(DEFAULT_MAX_FD_WRITE_BYTES),
            max_process_argv_bytes: Some(DEFAULT_MAX_PROCESS_ARGV_BYTES),
            max_process_env_bytes: Some(DEFAULT_MAX_PROCESS_ENV_BYTES),
            max_readdir_entries: Some(DEFAULT_MAX_READDIR_ENTRIES),
            max_wasm_fuel: None,
            max_wasm_memory_bytes: None,
            max_wasm_stack_bytes: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileSystemUsage {
    pub total_bytes: u64,
    pub inode_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceError {
    code: &'static str,
    message: String,
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
pub struct ResourceAccountant {
    limits: ResourceLimits,
}

impl ResourceAccountant {
    pub fn new(limits: ResourceLimits) -> Self {
        Self { limits }
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

        ResourceSnapshot {
            running_processes,
            exited_processes,
            fd_tables: fd_tables.len(),
            open_fds: fd_tables.total_open_fds(),
            pipes: pipes.pipe_count(),
            pipe_buffered_bytes: pipes.buffered_bytes(),
            ptys: ptys.pty_count(),
            pty_buffered_input_bytes: ptys.buffered_input_bytes(),
            pty_buffered_output_bytes: ptys.buffered_output_bytes(),
        }
    }

    pub fn check_process_spawn(
        &self,
        snapshot: &ResourceSnapshot,
        additional_fds: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_processes {
            if snapshot.running_processes >= limit {
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

    pub fn check_pread_length(&self, length: usize) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_pread_bytes {
            if length > limit {
                return Err(ResourceError::invalid_input(format!(
                    "pread length {length} exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn check_fd_write_size(&self, size: usize) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_fd_write_bytes {
            if size > limit {
                return Err(ResourceError::invalid_input(format!(
                    "write size {size} exceeds configured limit {limit}"
                )));
            }
        }

        Ok(())
    }

    pub fn max_readdir_entries(&self) -> Option<usize> {
        self.limits.max_readdir_entries
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

    fn check_open_fds(
        &self,
        snapshot: &ResourceSnapshot,
        additional_fds: usize,
    ) -> Result<(), ResourceError> {
        if let Some(limit) = self.limits.max_open_fds {
            if snapshot.open_fds.saturating_add(additional_fds) > limit {
                return Err(ResourceError::exhausted(
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
                    "maximum filesystem size limit reached",
                ));
            }
        }

        if let Some(limit) = self.limits.max_inode_count {
            if resulting_inodes > limit {
                return Err(ResourceError::filesystem_full(
                    "maximum inode count limit reached",
                ));
            }
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

pub fn measure_filesystem_usage<F: VirtualFileSystem>(
    filesystem: &mut F,
) -> VfsResult<FileSystemUsage> {
    let mut visited = BTreeSet::new();
    measure_path_usage(filesystem, "/", &mut visited)
}

fn measure_path_usage<F: VirtualFileSystem>(
    filesystem: &mut F,
    path: &str,
    visited: &mut BTreeSet<u64>,
) -> VfsResult<FileSystemUsage> {
    let stat = filesystem.lstat(path)?;
    let mut usage = FileSystemUsage::default();

    if visited.insert(stat.ino) {
        usage.inode_count += 1;
        if !stat.is_directory {
            usage.total_bytes = usage.total_bytes.saturating_add(stat.size);
        }
    }

    if !stat.is_directory || stat.is_symbolic_link {
        return Ok(usage);
    }

    for entry in filesystem.read_dir_with_types(path)? {
        if matches!(entry.name.as_str(), "." | "..") {
            continue;
        }

        let child_path = if path == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{path}/{}", entry.name)
        };
        let child_usage = measure_path_usage(filesystem, &child_path, visited)?;
        usage.total_bytes = usage.total_bytes.saturating_add(child_usage.total_bytes);
        usage.inode_count = usage.inode_count.saturating_add(child_usage.inode_count);
    }

    Ok(usage)
}
