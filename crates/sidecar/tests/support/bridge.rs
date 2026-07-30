use agentos_vm_host_interface::{
    ChmodRequest, ClockRequest, CommandPermissionRequest, CreateDirRequest,
    CreateJavascriptContextRequest, CreateWasmContextRequest, DiagnosticRecord, DirectoryEntry,
    EnvironmentPermissionRequest, ExecutionEvent, ExecutionHandleRequest, FileKind, FileMetadata,
    FilesystemPermissionRequest, FilesystemSnapshot, FlushFilesystemStateRequest,
    GuestContextHandle, GuestRuntime, HostClock, HostEvents, HostExecution, HostFilesystem,
    HostPermissions, HostPersistence, HostRandom, KillExecutionRequest, LifecycleEventRecord,
    LoadFilesystemStateRequest, LogRecord, NetworkPermissionRequest, PathRequest,
    PermissionDecision, PollExecutionEventRequest, RandomBytesRequest, ReadDirRequest,
    ReadFileRequest, RenameRequest, ScheduleTimerRequest, ScheduledTimer, StartExecutionRequest,
    StartedExecution, StructuredEventRecord, SymlinkRequest, TruncateRequest, VmHostTypes,
    WriteExecutionStdinRequest, WriteFileRequest,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubError {
    message: String,
}

impl StubError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn missing(kind: &'static str, key: &str) -> Self {
        Self {
            message: format!("missing {kind}: {key}"),
        }
    }

    fn invalid(kind: &'static str, key: &str) -> Self {
        Self {
            message: format!("invalid {kind}: {key}"),
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
    permission_responses: VecDeque<Result<PermissionDecision, StubError>>,
    worker_create_errors: VecDeque<StubError>,
    execution_start_errors: VecDeque<StubError>,
    pub filesystem_permission_requests: Vec<FilesystemPermissionRequest>,
    pub permission_checks: Vec<String>,
    pub log_events: Vec<LogRecord>,
    pub diagnostic_events: Vec<DiagnosticRecord>,
    pub structured_events: Vec<StructuredEventRecord>,
    pub lifecycle_events: Vec<LifecycleEventRecord>,
    pub scheduled_timers: Vec<ScheduleTimerRequest>,
    pub stdin_writes: Vec<WriteExecutionStdinRequest>,
    pub closed_executions: Vec<ExecutionHandleRequest>,
    pub killed_executions: Vec<KillExecutionRequest>,
    #[allow(dead_code)]
    pub terminated_workers: Vec<(String, String, String)>,
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
            permission_responses: VecDeque::new(),
            worker_create_errors: VecDeque::new(),
            execution_start_errors: VecDeque::new(),
            filesystem_permission_requests: Vec::new(),
            permission_checks: Vec::new(),
            log_events: Vec::new(),
            diagnostic_events: Vec::new(),
            structured_events: Vec::new(),
            lifecycle_events: Vec::new(),
            scheduled_timers: Vec::new(),
            stdin_writes: Vec::new(),
            closed_executions: Vec::new(),
            killed_executions: Vec::new(),
            terminated_workers: Vec::new(),
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

    pub fn push_permission_decision(&mut self, decision: PermissionDecision) {
        self.permission_responses.push_back(Ok(decision));
    }

    pub fn push_permission_error(&mut self, message: impl Into<String>) {
        self.permission_responses
            .push_back(Err(StubError::new(message)));
    }

    pub fn push_worker_create_error(&mut self, message: impl Into<String>) {
        self.worker_create_errors.push_back(StubError::new(message));
    }

    pub fn push_execution_start_error(&mut self, message: impl Into<String>) {
        self.execution_start_errors
            .push_back(StubError::new(message));
    }

    pub fn next_worker_create_error(&mut self) -> Option<StubError> {
        self.worker_create_errors.pop_front()
    }

    fn next_permission_response(&mut self) -> Result<PermissionDecision, StubError> {
        self.permission_responses
            .pop_front()
            .unwrap_or_else(|| Ok(PermissionDecision::allow()))
    }

    fn metadata_for_path(&self, path: &str, follow_links: bool) -> Result<FileMetadata, StubError> {
        let mut current_path = path.to_owned();
        let mut seen_links = BTreeSet::new();

        if follow_links {
            while let Some(target) = self.symlinks.get(&current_path) {
                if !seen_links.insert(current_path.clone()) {
                    return Err(StubError::invalid("symlink cycle", &current_path));
                }
                current_path = target.clone();
            }
        } else if self.symlinks.contains_key(&current_path) {
            return Ok(FileMetadata {
                mode: 0o777,
                size: 0,
                kind: FileKind::SymbolicLink,
            });
        }

        if let Some(bytes) = self.files.get(&current_path) {
            return Ok(FileMetadata {
                mode: 0o644,
                size: bytes.len() as u64,
                kind: FileKind::File,
            });
        }

        if let Some(entries) = self.directories.get(&current_path) {
            return Ok(FileMetadata {
                mode: 0o755,
                size: entries.len() as u64,
                kind: FileKind::Directory,
            });
        }

        Err(StubError::missing("path", &current_path))
    }
}

impl VmHostTypes for RecordingBridge {
    type Error = StubError;
}

impl HostFilesystem for RecordingBridge {
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

impl HostPermissions for RecordingBridge {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.filesystem_permission_requests.push(request.clone());
        self.permission_checks
            .push(format!("fs:{}:{}", request.vm_id, request.path));
        self.next_permission_response()
    }

    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("net:{}:{}", request.vm_id, request.resource));
        self.next_permission_response()
    }

    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("cmd:{}:{}", request.vm_id, request.command));
        self.next_permission_response()
    }

    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error> {
        self.permission_checks
            .push(format!("env:{}:{}", request.vm_id, request.key));
        self.next_permission_response()
    }
}

impl HostPersistence for RecordingBridge {
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

impl HostClock for RecordingBridge {
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

impl HostRandom for RecordingBridge {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error> {
        Ok(vec![0xA5; request.len])
    }
}

impl HostEvents for RecordingBridge {
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

impl HostExecution for RecordingBridge {
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
        if let Some(error) = self.execution_start_errors.pop_front() {
            return Err(error);
        }

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

#[test]
fn recording_bridge_rejects_symlink_cycles_when_following_metadata() {
    let mut bridge = RecordingBridge::default();
    bridge
        .symlink(SymlinkRequest {
            vm_id: String::from("vm-1"),
            target_path: String::from("/b"),
            link_path: String::from("/a"),
        })
        .expect("create first symlink");
    bridge
        .symlink(SymlinkRequest {
            vm_id: String::from("vm-1"),
            target_path: String::from("/a"),
            link_path: String::from("/b"),
        })
        .expect("create second symlink");

    let error = bridge
        .stat(PathRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/a"),
        })
        .expect_err("cycle should be rejected");
    assert!(error.message.contains("symlink cycle"));

    let metadata = bridge
        .lstat(PathRequest {
            vm_id: String::from("vm-1"),
            path: String::from("/a"),
        })
        .expect("lstat should not follow symlink");
    assert_eq!(metadata.kind, FileKind::SymbolicLink);
}
