use crate::fd_table::FdTableManager;
use crate::pipe_manager::PipeManager;
use crate::process_table::{ProcessStatus, ProcessTable};
use crate::pty::PtyManager;
use std::error::Error;
use std::fmt;

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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResourceLimits {
    pub max_processes: Option<usize>,
    pub max_open_fds: Option<usize>,
    pub max_pipes: Option<usize>,
    pub max_ptys: Option<usize>,
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
}
