use crate::{
    ChmodRequest, ClockRequest, CommandPermissionRequest, CreateDirRequest,
    CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord, DirectoryEntry,
    EnvironmentPermissionRequest, ExecutionEvent, ExecutionHandleRequest, FileKind, FileMetadata,
    FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    GuestContextHandle, HostClock, HostEvents, HostExecution, HostFilesystem, HostPermissions,
    HostPersistence, HostRandom, KillExecutionRequest, LifecycleEventRecord,
    LoadFilesystemStateRequest, LogRecord, NetworkPermissionRequest, PathRequest,
    PermissionDecision, PollExecutionEventRequest, RandomBytesRequest, ReadDirRequest,
    ReadFileRequest, RenameRequest, ScheduleTimerRequest, ScheduledTimer, StartExecutionRequest,
    StartedExecution, StructuredEventRecord, SymlinkRequest, TruncateRequest, VmHostTypes,
    WriteExecutionStdinRequest, WriteFileRequest,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// Default in-process host implementation for an embedded VM.
///
/// VM state remains kernel-owned. This adapter supplies ambient host callbacks
/// only when an embedder has not injected a more restrictive implementation.
#[derive(Debug, Clone)]
pub struct LocalVmHost {
    started_at: Instant,
    next_timer_id: usize,
    snapshots: BTreeMap<String, FilesystemSnapshot>,
}

impl Default for LocalVmHost {
    fn default() -> Self {
        Self {
            started_at: Instant::now(),
            next_timer_id: 0,
            snapshots: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalVmHostError {
    message: String,
}

impl LocalVmHostError {
    fn unsupported(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn io(operation: &str, path: &str, error: io::Error) -> Self {
        Self::unsupported(format!("{operation} {path}: {error}"))
    }
}

impl fmt::Display for LocalVmHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for LocalVmHostError {}

impl LocalVmHost {
    fn host_path(path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(candidate)
        }
    }

    fn file_metadata(metadata: fs::Metadata) -> FileMetadata {
        FileMetadata {
            mode: metadata.permissions().mode(),
            size: metadata.size(),
            kind: Self::file_kind(metadata.file_type()),
        }
    }

    fn file_kind(file_type: fs::FileType) -> FileKind {
        if file_type.is_file() {
            FileKind::File
        } else if file_type.is_dir() {
            FileKind::Directory
        } else if file_type.is_symlink() {
            FileKind::SymbolicLink
        } else {
            FileKind::Other
        }
    }
}

impl VmHostTypes for LocalVmHost {
    type Error = LocalVmHostError;
}

impl HostFilesystem for LocalVmHost {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
        fs::read(Self::host_path(&request.path))
            .map_err(|error| LocalVmHostError::io("read", &request.path, error))
    }

    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalVmHostError::io("mkdir", &request.path, error))?;
        }
        fs::write(host_path, request.contents)
            .map_err(|error| LocalVmHostError::io("write", &request.path, error))
    }

    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalVmHostError::io("stat", &request.path, error))
    }

    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        fs::symlink_metadata(Self::host_path(&request.path))
            .map(Self::file_metadata)
            .map_err(|error| LocalVmHostError::io("lstat", &request.path, error))
    }

    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error> {
        let mut entries = fs::read_dir(Self::host_path(&request.path))
            .map_err(|error| LocalVmHostError::io("readdir", &request.path, error))?
            .map(|entry| {
                let entry =
                    entry.map_err(|error| LocalVmHostError::io("readdir", &request.path, error))?;
                let kind = entry
                    .file_type()
                    .map(Self::file_kind)
                    .map_err(|error| LocalVmHostError::io("readdir", &request.path, error))?;
                Ok(DirectoryEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    kind,
                })
            })
            .collect::<Result<Vec<_>, LocalVmHostError>>()?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error> {
        let host_path = Self::host_path(&request.path);
        if request.recursive {
            fs::create_dir_all(host_path)
        } else {
            fs::create_dir(host_path)
        }
        .map_err(|error| LocalVmHostError::io("mkdir", &request.path, error))
    }

    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_file(Self::host_path(&request.path))
            .map_err(|error| LocalVmHostError::io("unlink", &request.path, error))
    }

    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        fs::remove_dir(Self::host_path(&request.path))
            .map_err(|error| LocalVmHostError::io("rmdir", &request.path, error))
    }

    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error> {
        let destination = Self::host_path(&request.to_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalVmHostError::io("mkdir", &request.to_path, error))?;
        }
        fs::rename(Self::host_path(&request.from_path), destination).map_err(|error| {
            LocalVmHostError::unsupported(format!(
                "rename {} -> {}: {error}",
                request.from_path, request.to_path
            ))
        })
    }

    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error> {
        let link_path = Self::host_path(&request.link_path);
        if let Some(parent) = link_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| LocalVmHostError::io("mkdir", &request.link_path, error))?;
        }
        symlink(&request.target_path, link_path)
            .map_err(|error| LocalVmHostError::io("symlink", &request.link_path, error))
    }

    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error> {
        fs::read_link(Self::host_path(&request.path))
            .map(|target| target.to_string_lossy().into_owned())
            .map_err(|error| LocalVmHostError::io("readlink", &request.path, error))
    }

    fn chmod(&mut self, request: ChmodRequest) -> Result<(), Self::Error> {
        fs::set_permissions(
            Self::host_path(&request.path),
            fs::Permissions::from_mode(request.mode),
        )
        .map_err(|error| LocalVmHostError::io("chmod", &request.path, error))
    }

    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error> {
        OpenOptions::new()
            .write(true)
            .open(Self::host_path(&request.path))
            .and_then(|file| file.set_len(request.len))
            .map_err(|error| LocalVmHostError::io("truncate", &request.path, error))
    }

    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error> {
        Ok(fs::symlink_metadata(Self::host_path(&request.path)).is_ok())
    }
}

impl HostPermissions for LocalVmHost {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no host filesystem policy registered for {}:{}",
            request.vm_id, request.path
        )))
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no host network policy registered for {}:{}",
            request.vm_id, request.resource
        )))
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no host command policy registered for {}:{}",
            request.vm_id, request.command
        )))
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        Ok(PermissionDecision::deny(format!(
            "no host environment policy registered for {}:{}",
            request.vm_id, request.key
        )))
    }
}

impl HostPersistence for LocalVmHost {
    fn load_filesystem_state(
        &mut self,
        request: LoadFilesystemStateRequest,
    ) -> Result<Option<FilesystemSnapshot>, Self::Error> {
        Ok(self.snapshots.get(&request.vm_id).cloned())
    }

    fn flush_filesystem_state(
        &mut self,
        request: FlushFilesystemStateRequest,
    ) -> Result<(), Self::Error> {
        self.snapshots.insert(request.vm_id, request.snapshot);
        Ok(())
    }
}

impl HostClock for LocalVmHost {
    fn wall_clock(&mut self, _request: ClockRequest) -> Result<SystemTime, Self::Error> {
        Ok(SystemTime::now())
    }

    fn monotonic_clock(&mut self, _request: ClockRequest) -> Result<Duration, Self::Error> {
        Ok(self.started_at.elapsed())
    }

    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error> {
        self.next_timer_id = self.next_timer_id.saturating_add(1);
        Ok(ScheduledTimer {
            timer_id: format!("timer-{}", self.next_timer_id),
            delay: request.delay,
        })
    }
}

impl HostRandom for LocalVmHost {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        let mut bytes = vec![0; request.len];
        getrandom::getrandom(&mut bytes)
            .map_err(|error| LocalVmHostError::unsupported(format!("host randomness: {error}")))?;
        Ok(bytes)
    }
}

impl HostEvents for LocalVmHost {
    fn emit_structured_event(&mut self, _event: StructuredEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_diagnostic(&mut self, _event: DiagnosticRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_log(&mut self, _event: LogRecord) -> Result<(), Self::Error> {
        Ok(())
    }

    fn emit_lifecycle(&mut self, _event: LifecycleEventRecord) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl HostExecution for LocalVmHost {
    fn create_javascript_context(
        &mut self,
        _request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn create_wasm_context(
        &mut self,
        _request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn start_execution(
        &mut self,
        _request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn write_stdin(&mut self, _request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn close_stdin(&mut self, _request: ExecutionHandleRequest) -> Result<(), Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn kill_execution(&mut self, _request: KillExecutionRequest) -> Result<(), Self::Error> {
        Err(LocalVmHostError::unsupported(
            "no host execution adapter is registered",
        ))
    }

    fn poll_execution_event(
        &mut self,
        _request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error> {
        Ok(None)
    }
}
