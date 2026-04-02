use agent_os_kernel::bridge::{
    BridgeTypes, ChmodRequest, ClockBridge, ClockRequest, CommandPermissionRequest,
    CreateDirRequest, CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord,
    DirectoryEntry, EnvironmentPermissionRequest, EventBridge, ExecutionBridge, ExecutionEvent,
    ExecutionHandleRequest, FileKind, FileMetadata, FilesystemBridge, FilesystemPermissionRequest,
    FilesystemSnapshot, FlushFilesystemStateRequest, GuestContextHandle, GuestRuntime,
    KillExecutionRequest, LifecycleEventRecord, LoadFilesystemStateRequest, LogRecord,
    NetworkPermissionRequest, PathRequest, PermissionBridge, PermissionDecision, PersistenceBridge,
    PollExecutionEventRequest, RandomBridge, RandomBytesRequest, ReadDirRequest, ReadFileRequest,
    RenameRequest, ScheduleTimerRequest, ScheduledTimer, StartExecutionRequest, StartedExecution,
    StructuredEventRecord, SymlinkRequest, TruncateRequest, WriteExecutionStdinRequest,
    WriteFileRequest,
};
use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubError {
    message: String,
}

impl StubError {
    fn missing(kind: &'static str, key: &str) -> Self {
        Self {
            message: format!("missing {kind}: {key}"),
        }
    }
}

#[derive(Debug)]
pub struct RecordingBridge {
    next_context_id: usize,
    next_execution_id: usize,
    next_timer_id: usize,
    files: BTreeMap<String, Vec<u8>>,
    directories: BTreeMap<String, Vec<DirectoryEntry>>,
    symlinks: BTreeMap<String, String>,
    snapshots: BTreeMap<String, FilesystemSnapshot>,
    execution_events: VecDeque<ExecutionEvent>,
    pub permission_checks: Vec<String>,
    pub log_events: Vec<LogRecord>,
    pub diagnostic_events: Vec<DiagnosticRecord>,
    pub structured_events: Vec<StructuredEventRecord>,
    pub lifecycle_events: Vec<LifecycleEventRecord>,
    pub scheduled_timers: Vec<ScheduleTimerRequest>,
    pub stdin_writes: Vec<WriteExecutionStdinRequest>,
    pub closed_executions: Vec<ExecutionHandleRequest>,
    pub killed_executions: Vec<KillExecutionRequest>,
}

impl Default for RecordingBridge {
    fn default() -> Self {
        let mut directories = BTreeMap::new();
        directories.insert(String::from("/"), Vec::new());

        Self {
            next_context_id: 1,
            next_execution_id: 1,
            next_timer_id: 1,
            files: BTreeMap::new(),
            directories,
            symlinks: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            execution_events: VecDeque::new(),
            permission_checks: Vec::new(),
            log_events: Vec::new(),
            diagnostic_events: Vec::new(),
            structured_events: Vec::new(),
            lifecycle_events: Vec::new(),
            scheduled_timers: Vec::new(),
            stdin_writes: Vec::new(),
            closed_executions: Vec::new(),
            killed_executions: Vec::new(),
        }
    }
}

#[allow(dead_code)]
impl RecordingBridge {
    pub fn seed_file(&mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) {
        self.files.insert(path.into(), contents.into());
    }

    pub fn seed_directory(&mut self, path: impl Into<String>, entries: Vec<DirectoryEntry>) {
        self.directories.insert(path.into(), entries);
    }

    pub fn seed_snapshot(&mut self, vm_id: impl Into<String>, snapshot: FilesystemSnapshot) {
        self.snapshots.insert(vm_id.into(), snapshot);
    }

    pub fn push_execution_event(&mut self, event: ExecutionEvent) {
        self.execution_events.push_back(event);
    }

    fn metadata_for_path(&self, path: &str, follow_links: bool) -> Result<FileMetadata, StubError> {
        if follow_links {
            if let Some(target) = self.symlinks.get(path) {
                return self.metadata_for_path(target, true);
            }
        } else if self.symlinks.contains_key(path) {
            return Ok(FileMetadata {
                mode: 0o777,
                size: 0,
                kind: FileKind::SymbolicLink,
            });
        }

        if let Some(bytes) = self.files.get(path) {
            return Ok(FileMetadata {
                mode: 0o644,
                size: bytes.len() as u64,
                kind: FileKind::File,
            });
        }

        if let Some(entries) = self.directories.get(path) {
            return Ok(FileMetadata {
                mode: 0o755,
                size: entries.len() as u64,
                kind: FileKind::Directory,
            });
        }

        Err(StubError::missing("path", path))
    }
}

impl BridgeTypes for RecordingBridge {
    type Error = StubError;
}

impl FilesystemBridge for RecordingBridge {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error> {
        self.files
            .get(&request.path)
            .cloned()
            .ok_or_else(|| StubError::missing("file", &request.path))
    }

    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error> {
        self.files.insert(request.path, request.contents);
        Ok(())
    }

    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        self.metadata_for_path(&request.path, true)
    }

    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error> {
        self.metadata_for_path(&request.path, false)
    }

    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error> {
        Ok(self
            .directories
            .get(&request.path)
            .cloned()
            .unwrap_or_default())
    }

    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error> {
        self.directories.entry(request.path).or_default();
        Ok(())
    }

    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        self.files.remove(&request.path);
        Ok(())
    }

    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error> {
        self.directories.remove(&request.path);
        Ok(())
    }

    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error> {
        if let Some(bytes) = self.files.remove(&request.from_path) {
            self.files.insert(request.to_path, bytes);
            return Ok(());
        }

        if let Some(target) = self.symlinks.remove(&request.from_path) {
            self.symlinks.insert(request.to_path, target);
            return Ok(());
        }

        if let Some(entries) = self.directories.remove(&request.from_path) {
            self.directories.insert(request.to_path, entries);
            return Ok(());
        }

        Err(StubError::missing("rename source", &request.from_path))
    }

    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error> {
        self.symlinks.insert(request.link_path, request.target_path);
        Ok(())
    }

    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error> {
        self.symlinks
            .get(&request.path)
            .cloned()
            .ok_or_else(|| StubError::missing("symlink", &request.path))
    }

    fn chmod(&mut self, _request: ChmodRequest) -> Result<(), Self::Error> {
        Ok(())
    }

    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error> {
        let Some(bytes) = self.files.get_mut(&request.path) else {
            return Err(StubError::missing("file", &request.path));
        };

        bytes.resize(request.len as usize, 0);
        Ok(())
    }

    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error> {
        Ok(self.files.contains_key(&request.path)
            || self.directories.contains_key(&request.path)
            || self.symlinks.contains_key(&request.path))
    }
}

impl PermissionBridge for RecordingBridge {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("fs:{}:{}", request.vm_id, request.path));
        Ok(PermissionDecision::allow())
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("net:{}:{}", request.vm_id, request.resource));
        Ok(PermissionDecision::allow())
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("cmd:{}:{}", request.vm_id, request.command));
        Ok(PermissionDecision::allow())
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("env:{}:{}", request.vm_id, request.key));
        Ok(PermissionDecision::allow())
    }
}

impl PersistenceBridge for RecordingBridge {
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

impl ClockBridge for RecordingBridge {
    fn wall_clock(&mut self, _request: ClockRequest) -> Result<SystemTime, Self::Error> {
        Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(1_710_000_000))
    }

    fn monotonic_clock(&mut self, _request: ClockRequest) -> Result<Duration, Self::Error> {
        Ok(Duration::from_millis(42))
    }

    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error> {
        self.scheduled_timers.push(request.clone());

        let timer = ScheduledTimer {
            timer_id: format!("timer-{}", self.next_timer_id),
            delay: request.delay,
        };
        self.next_timer_id += 1;

        Ok(timer)
    }
}

impl RandomBridge for RecordingBridge {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0xA5; request.len])
    }
}

impl EventBridge for RecordingBridge {
    fn emit_structured_event(&mut self, event: StructuredEventRecord) -> Result<(), Self::Error> {
        self.structured_events.push(event);
        Ok(())
    }

    fn emit_diagnostic(&mut self, event: DiagnosticRecord) -> Result<(), Self::Error> {
        self.diagnostic_events.push(event);
        Ok(())
    }

    fn emit_log(&mut self, event: LogRecord) -> Result<(), Self::Error> {
        self.log_events.push(event);
        Ok(())
    }

    fn emit_lifecycle(&mut self, event: LifecycleEventRecord) -> Result<(), Self::Error> {
        self.lifecycle_events.push(event);
        Ok(())
    }
}

impl ExecutionBridge for RecordingBridge {
    fn create_javascript_context(
        &mut self,
        _request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        let handle = GuestContextHandle {
            context_id: format!("js-context-{}", self.next_context_id),
            runtime: GuestRuntime::JavaScript,
        };
        self.next_context_id += 1;
        Ok(handle)
    }

    fn create_wasm_context(
        &mut self,
        _request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error> {
        let handle = GuestContextHandle {
            context_id: format!("wasm-context-{}", self.next_context_id),
            runtime: GuestRuntime::WebAssembly,
        };
        self.next_context_id += 1;
        Ok(handle)
    }

    fn start_execution(
        &mut self,
        _request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error> {
        let execution = StartedExecution {
            execution_id: format!("exec-{}", self.next_execution_id),
        };
        self.next_execution_id += 1;
        Ok(execution)
    }

    fn write_stdin(&mut self, request: WriteExecutionStdinRequest) -> Result<(), Self::Error> {
        self.stdin_writes.push(request);
        Ok(())
    }

    fn close_stdin(&mut self, request: ExecutionHandleRequest) -> Result<(), Self::Error> {
        self.closed_executions.push(request);
        Ok(())
    }

    fn kill_execution(&mut self, request: KillExecutionRequest) -> Result<(), Self::Error> {
        self.killed_executions.push(request);
        Ok(())
    }

    fn poll_execution_event(
        &mut self,
        _request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error> {
        Ok(self.execution_events.pop_front())
    }
}
