#![forbid(unsafe_code)]

//! Shared bridge contracts between the Agent OS kernel and execution planes.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// Shared associated types for bridge implementations.
pub trait BridgeTypes {
    type Error;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    SymbolicLink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub mode: u32,
    pub size: u64,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: FileKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRequest {
    pub vm_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadFileRequest {
    pub vm_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFileRequest {
    pub vm_id: String,
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadDirRequest {
    pub vm_id: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDirRequest {
    pub vm_id: String,
    pub path: String,
    pub recursive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameRequest {
    pub vm_id: String,
    pub from_path: String,
    pub to_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymlinkRequest {
    pub vm_id: String,
    pub target_path: String,
    pub link_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChmodRequest {
    pub vm_id: String,
    pub path: String,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TruncateRequest {
    pub vm_id: String,
    pub path: String,
    pub len: u64,
}

pub trait FilesystemBridge: BridgeTypes {
    fn read_file(&mut self, request: ReadFileRequest) -> Result<Vec<u8>, Self::Error>;
    fn write_file(&mut self, request: WriteFileRequest) -> Result<(), Self::Error>;
    fn stat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error>;
    fn lstat(&mut self, request: PathRequest) -> Result<FileMetadata, Self::Error>;
    fn read_dir(&mut self, request: ReadDirRequest) -> Result<Vec<DirectoryEntry>, Self::Error>;
    fn create_dir(&mut self, request: CreateDirRequest) -> Result<(), Self::Error>;
    fn remove_file(&mut self, request: PathRequest) -> Result<(), Self::Error>;
    fn remove_dir(&mut self, request: PathRequest) -> Result<(), Self::Error>;
    fn rename(&mut self, request: RenameRequest) -> Result<(), Self::Error>;
    fn symlink(&mut self, request: SymlinkRequest) -> Result<(), Self::Error>;
    fn read_link(&mut self, request: PathRequest) -> Result<String, Self::Error>;
    fn chmod(&mut self, request: ChmodRequest) -> Result<(), Self::Error>;
    fn truncate(&mut self, request: TruncateRequest) -> Result<(), Self::Error>;
    fn exists(&mut self, request: PathRequest) -> Result<bool, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionVerdict {
    Allow,
    Deny,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub verdict: PermissionVerdict,
    pub reason: Option<String>,
}

impl PermissionDecision {
    pub fn allow() -> Self {
        Self {
            verdict: PermissionVerdict::Allow,
            reason: None,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            verdict: PermissionVerdict::Deny,
            reason: Some(reason.into()),
        }
    }

    pub fn prompt(reason: impl Into<String>) -> Self {
        Self {
            verdict: PermissionVerdict::Prompt,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemAccess {
    Read,
    Write,
    Stat,
    ReadDir,
    CreateDir,
    Remove,
    Rename,
    Symlink,
    ReadLink,
    Chmod,
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemPermissionRequest {
    pub vm_id: String,
    pub path: String,
    pub access: FilesystemAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAccess {
    Fetch,
    Http,
    Dns,
    Listen,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkPermissionRequest {
    pub vm_id: String,
    pub access: NetworkAccess,
    pub resource: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPermissionRequest {
    pub vm_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentPermissionRequest {
    pub vm_id: String,
    pub access: EnvironmentAccess,
    pub key: String,
    pub value: Option<String>,
}

pub trait PermissionBridge: BridgeTypes {
    fn check_filesystem_access(
        &mut self,
        request: FilesystemPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error>;
    fn check_network_access(
        &mut self,
        request: NetworkPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error>;
    fn check_command_execution(
        &mut self,
        request: CommandPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error>;
    fn check_environment_access(
        &mut self,
        request: EnvironmentPermissionRequest,
    ) -> Result<PermissionDecision, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemSnapshot {
    pub format: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFilesystemStateRequest {
    pub vm_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushFilesystemStateRequest {
    pub vm_id: String,
    pub snapshot: FilesystemSnapshot,
}

pub trait PersistenceBridge: BridgeTypes {
    fn load_filesystem_state(
        &mut self,
        request: LoadFilesystemStateRequest,
    ) -> Result<Option<FilesystemSnapshot>, Self::Error>;
    fn flush_filesystem_state(
        &mut self,
        request: FlushFilesystemStateRequest,
    ) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockRequest {
    pub vm_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleTimerRequest {
    pub vm_id: String,
    pub delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTimer {
    pub timer_id: String,
    pub delay: Duration,
}

pub trait ClockBridge: BridgeTypes {
    fn wall_clock(&mut self, request: ClockRequest) -> Result<SystemTime, Self::Error>;
    fn monotonic_clock(&mut self, request: ClockRequest) -> Result<Duration, Self::Error>;
    fn schedule_timer(
        &mut self,
        request: ScheduleTimerRequest,
    ) -> Result<ScheduledTimer, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomBytesRequest {
    pub vm_id: String,
    pub len: usize,
}

pub trait RandomBridge: BridgeTypes {
    fn fill_random_bytes(&mut self, request: RandomBytesRequest) -> Result<Vec<u8>, Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    pub vm_id: String,
    pub level: LogLevel,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticRecord {
    pub vm_id: String,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredEventRecord {
    pub vm_id: String,
    pub name: String,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Starting,
    Ready,
    Busy,
    Terminated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleEventRecord {
    pub vm_id: String,
    pub state: LifecycleState,
    pub detail: Option<String>,
}

pub trait EventBridge: BridgeTypes {
    fn emit_structured_event(&mut self, event: StructuredEventRecord) -> Result<(), Self::Error>;
    fn emit_diagnostic(&mut self, event: DiagnosticRecord) -> Result<(), Self::Error>;
    fn emit_log(&mut self, event: LogRecord) -> Result<(), Self::Error>;
    fn emit_lifecycle(&mut self, event: LifecycleEventRecord) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestRuntime {
    JavaScript,
    WebAssembly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateJavascriptContextRequest {
    pub vm_id: String,
    pub bootstrap_module: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWasmContextRequest {
    pub vm_id: String,
    pub module_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestContextHandle {
    pub context_id: String,
    pub runtime: GuestRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartExecutionRequest {
    pub vm_id: String,
    pub context_id: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedExecution {
    pub execution_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionHandleRequest {
    pub vm_id: String,
    pub execution_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteExecutionStdinRequest {
    pub vm_id: String,
    pub execution_id: String,
    pub chunk: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionSignal {
    Terminate,
    Interrupt,
    Kill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KillExecutionRequest {
    pub vm_id: String,
    pub execution_id: String,
    pub signal: ExecutionSignal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollExecutionEventRequest {
    pub vm_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputChunk {
    pub vm_id: String,
    pub execution_id: String,
    pub chunk: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionExited {
    pub vm_id: String,
    pub execution_id: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestKernelCall {
    pub vm_id: String,
    pub execution_id: String,
    pub operation: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEvent {
    Stdout(OutputChunk),
    Stderr(OutputChunk),
    Exited(ExecutionExited),
    GuestRequest(GuestKernelCall),
}

pub trait ExecutionBridge: BridgeTypes {
    fn create_javascript_context(
        &mut self,
        request: CreateJavascriptContextRequest,
    ) -> Result<GuestContextHandle, Self::Error>;
    fn create_wasm_context(
        &mut self,
        request: CreateWasmContextRequest,
    ) -> Result<GuestContextHandle, Self::Error>;
    fn start_execution(
        &mut self,
        request: StartExecutionRequest,
    ) -> Result<StartedExecution, Self::Error>;
    fn write_stdin(&mut self, request: WriteExecutionStdinRequest) -> Result<(), Self::Error>;
    fn close_stdin(&mut self, request: ExecutionHandleRequest) -> Result<(), Self::Error>;
    fn kill_execution(&mut self, request: KillExecutionRequest) -> Result<(), Self::Error>;
    fn poll_execution_event(
        &mut self,
        request: PollExecutionEventRequest,
    ) -> Result<Option<ExecutionEvent>, Self::Error>;
}

pub trait HostBridge:
    FilesystemBridge
    + PermissionBridge
    + PersistenceBridge
    + ClockBridge
    + RandomBridge
    + EventBridge
    + ExecutionBridge
{
}

impl<T> HostBridge for T where
    T: FilesystemBridge
        + PermissionBridge
        + PersistenceBridge
        + ClockBridge
        + RandomBridge
        + EventBridge
        + ExecutionBridge
{
}
