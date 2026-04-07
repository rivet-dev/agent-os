use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;

pub const PROTOCOL_NAME: &str = "agent-os-sidecar";
pub const PROTOCOL_VERSION: u16 = 1;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const DEFAULT_COMPLETED_RESPONSE_CAP: usize = 10_000;
pub type RequestId = i64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolSchema {
    pub name: String,
    pub version: u16,
}

impl ProtocolSchema {
    pub fn current() -> Self {
        Self {
            name: PROTOCOL_NAME.to_string(),
            version: PROTOCOL_VERSION,
        }
    }
}

impl Default for ProtocolSchema {
    fn default() -> Self {
        Self::current()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum OwnershipScope {
    Connection {
        connection_id: String,
    },
    Session {
        connection_id: String,
        session_id: String,
    },
    Vm {
        connection_id: String,
        session_id: String,
        vm_id: String,
    },
}

impl OwnershipScope {
    pub fn connection(connection_id: impl Into<String>) -> Self {
        Self::Connection {
            connection_id: connection_id.into(),
        }
    }

    pub fn session(connection_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self::Session {
            connection_id: connection_id.into(),
            session_id: session_id.into(),
        }
    }

    pub fn vm(
        connection_id: impl Into<String>,
        session_id: impl Into<String>,
        vm_id: impl Into<String>,
    ) -> Self {
        Self::Vm {
            connection_id: connection_id.into(),
            session_id: session_id.into(),
            vm_id: vm_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "frame_type", rename_all = "snake_case")]
pub enum ProtocolFrame {
    Request(RequestFrame),
    Response(ResponseFrame),
    Event(EventFrame),
    SidecarRequest(SidecarRequestFrame),
    SidecarResponse(SidecarResponseFrame),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub schema: ProtocolSchema,
    pub request_id: RequestId,
    pub ownership: OwnershipScope,
    pub payload: RequestPayload,
}

impl RequestFrame {
    pub fn new(request_id: RequestId, ownership: OwnershipScope, payload: RequestPayload) -> Self {
        Self {
            schema: ProtocolSchema::current(),
            request_id,
            ownership,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub schema: ProtocolSchema,
    pub request_id: RequestId,
    pub ownership: OwnershipScope,
    pub payload: ResponsePayload,
}

impl ResponseFrame {
    pub fn new(request_id: RequestId, ownership: OwnershipScope, payload: ResponsePayload) -> Self {
        Self {
            schema: ProtocolSchema::current(),
            request_id,
            ownership,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarRequestFrame {
    pub schema: ProtocolSchema,
    pub request_id: RequestId,
    pub ownership: OwnershipScope,
    pub payload: SidecarRequestPayload,
}

impl SidecarRequestFrame {
    pub fn new(
        request_id: RequestId,
        ownership: OwnershipScope,
        payload: SidecarRequestPayload,
    ) -> Self {
        Self {
            schema: ProtocolSchema::current(),
            request_id,
            ownership,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarResponseFrame {
    pub schema: ProtocolSchema,
    pub request_id: RequestId,
    pub ownership: OwnershipScope,
    pub payload: SidecarResponsePayload,
}

impl SidecarResponseFrame {
    pub fn new(
        request_id: RequestId,
        ownership: OwnershipScope,
        payload: SidecarResponsePayload,
    ) -> Self {
        Self {
            schema: ProtocolSchema::current(),
            request_id,
            ownership,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFrame {
    pub schema: ProtocolSchema,
    pub ownership: OwnershipScope,
    pub payload: EventPayload,
}

impl EventFrame {
    pub fn new(ownership: OwnershipScope, payload: EventPayload) -> Self {
        Self {
            schema: ProtocolSchema::current(),
            ownership,
            payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RequestPayload {
    Authenticate(AuthenticateRequest),
    OpenSession(OpenSessionRequest),
    CreateVm(CreateVmRequest),
    DisposeVm(DisposeVmRequest),
    BootstrapRootFilesystem(BootstrapRootFilesystemRequest),
    ConfigureVm(ConfigureVmRequest),
    GuestFilesystemCall(GuestFilesystemCallRequest),
    SnapshotRootFilesystem(SnapshotRootFilesystemRequest),
    Execute(ExecuteRequest),
    WriteStdin(WriteStdinRequest),
    CloseStdin(CloseStdinRequest),
    KillProcess(KillProcessRequest),
    FindListener(FindListenerRequest),
    FindBoundUdp(FindBoundUdpRequest),
    GetSignalState(GetSignalStateRequest),
    GetZombieTimerCount(GetZombieTimerCountRequest),
    HostFilesystemCall(HostFilesystemCallRequest),
    PermissionRequest(PermissionRequest),
    PersistenceLoad(PersistenceLoadRequest),
    PersistenceFlush(PersistenceFlushRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsePayload {
    Authenticated(AuthenticatedResponse),
    SessionOpened(SessionOpenedResponse),
    VmCreated(VmCreatedResponse),
    VmDisposed(VmDisposedResponse),
    RootFilesystemBootstrapped(RootFilesystemBootstrappedResponse),
    VmConfigured(VmConfiguredResponse),
    GuestFilesystemResult(GuestFilesystemResultResponse),
    RootFilesystemSnapshot(RootFilesystemSnapshotResponse),
    ProcessStarted(ProcessStartedResponse),
    StdinWritten(StdinWrittenResponse),
    StdinClosed(StdinClosedResponse),
    ProcessKilled(ProcessKilledResponse),
    ListenerSnapshot(ListenerSnapshotResponse),
    BoundUdpSnapshot(BoundUdpSnapshotResponse),
    SignalState(SignalStateResponse),
    ZombieTimerCount(ZombieTimerCountResponse),
    FilesystemResult(FilesystemResultResponse),
    PermissionDecision(PermissionDecisionResponse),
    PersistenceState(PersistenceStateResponse),
    PersistenceFlushed(PersistenceFlushedResponse),
    Rejected(RejectedResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SidecarRequestPayload {
    ToolInvocation(ToolInvocationRequest),
    JsBridgeCall(JsBridgeCallRequest),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SidecarResponsePayload {
    ToolInvocationResult(ToolInvocationResultResponse),
    JsBridgeResult(JsBridgeResultResponse),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    VmLifecycle(VmLifecycleEvent),
    ProcessOutput(ProcessOutputEvent),
    ProcessExited(ProcessExitedEvent),
    Structured(StructuredEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidecarPlacement {
    Shared { pool: Option<String> },
    Explicit { sidecar_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestRuntimeKind {
    JavaScript,
    Python,
    WebAssembly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisposeReason {
    Requested,
    ConnectionClosed,
    HostShutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemOperation {
    Read,
    Write,
    Stat,
    ReadDir,
    Mkdir,
    Remove,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuestFilesystemOperation {
    ReadFile,
    WriteFile,
    CreateDir,
    Mkdir,
    Exists,
    Stat,
    Lstat,
    ReadDir,
    RemoveFile,
    RemoveDir,
    Rename,
    Realpath,
    Symlink,
    ReadLink,
    Link,
    Chmod,
    Chown,
    Utimes,
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RootFilesystemEntryKind {
    #[default]
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RootFilesystemMode {
    #[default]
    Ephemeral,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RootFilesystemLowerDescriptor {
    Snapshot { entries: Vec<RootFilesystemEntry> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamChannel {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmLifecycleState {
    Creating,
    Ready,
    Disposing,
    Disposed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticateRequest {
    pub client_name: String,
    pub auth_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSessionRequest {
    pub placement: SidecarPlacement,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVmRequest {
    pub runtime: GuestRuntimeKind,
    pub metadata: BTreeMap<String, String>,
    #[serde(default)]
    pub root_filesystem: RootFilesystemDescriptor,
    #[serde(default)]
    pub permissions: Vec<PermissionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisposeVmRequest {
    pub reason: DisposeReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootstrapRootFilesystemRequest {
    pub entries: Vec<RootFilesystemEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RootFilesystemDescriptor {
    #[serde(default)]
    pub mode: RootFilesystemMode,
    #[serde(default)]
    pub disable_default_base_layer: bool,
    #[serde(default)]
    pub lowers: Vec<RootFilesystemLowerDescriptor>,
    #[serde(default)]
    pub bootstrap_entries: Vec<RootFilesystemEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootFilesystemEntryEncoding {
    Utf8,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RootFilesystemEntry {
    pub path: String,
    pub kind: RootFilesystemEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<RootFilesystemEntryEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureVmRequest {
    pub mounts: Vec<MountDescriptor>,
    pub software: Vec<SoftwareDescriptor>,
    pub permissions: Vec<PermissionDescriptor>,
    pub instructions: Vec<String>,
    pub projected_modules: Vec<ProjectedModuleDescriptor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub command_permissions: BTreeMap<String, WasmPermissionTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFilesystemCallRequest {
    pub operation: GuestFilesystemOperation,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<RootFilesystemEntryEncoding>,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atime_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub len: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SnapshotRootFilesystemRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountDescriptor {
    pub guest_path: String,
    pub read_only: bool,
    pub plugin: MountPluginDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPluginDescriptor {
    pub id: String,
    #[serde(default)]
    pub config: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftwareDescriptor {
    pub package_name: String,
    pub root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDescriptor {
    pub capability: String,
    pub mode: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectedModuleDescriptor {
    pub package_name: String,
    pub entrypoint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasmPermissionTier {
    Full,
    ReadWrite,
    ReadOnly,
    Isolated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub process_id: String,
    pub runtime: GuestRuntimeKind,
    pub entrypoint: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm_permission_tier: Option<WasmPermissionTier>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteStdinRequest {
    pub process_id: String,
    pub chunk: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseStdinRequest {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillProcessRequest {
    pub process_id: String,
    pub signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FindListenerRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FindBoundUdpRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSignalStateRequest {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct GetZombieTimerCountRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFilesystemCallRequest {
    pub operation: FilesystemOperation,
    pub path: String,
    pub payload_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub capability: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceLoadRequest {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceFlushRequest {
    pub key: String,
    pub payload_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationRequest {
    pub invocation_id: String,
    pub tool_key: String,
    pub input: Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsBridgeCallRequest {
    pub call_id: String,
    pub mount_id: String,
    pub operation: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthenticatedResponse {
    pub sidecar_id: String,
    pub connection_id: String,
    pub max_frame_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOpenedResponse {
    pub session_id: String,
    pub owner_connection_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmCreatedResponse {
    pub vm_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmDisposedResponse {
    pub vm_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFilesystemBootstrappedResponse {
    pub entry_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmConfiguredResponse {
    pub applied_mounts: u32,
    pub applied_software: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFilesystemStat {
    pub mode: u32,
    pub size: u64,
    pub blocks: u64,
    pub dev: u64,
    pub rdev: u64,
    pub is_directory: bool,
    pub is_symbolic_link: bool,
    pub atime_ms: u64,
    pub mtime_ms: u64,
    pub ctime_ms: u64,
    pub birthtime_ms: u64,
    pub ino: u64,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestFilesystemResultResponse {
    pub operation: GuestFilesystemOperation,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<RootFilesystemEntryEncoding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stat: Option<GuestFilesystemStat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFilesystemSnapshotResponse {
    pub entries: Vec<RootFilesystemEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessStartedResponse {
    pub process_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinWrittenResponse {
    pub process_id: String,
    pub accepted_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdinClosedResponse {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessKilledResponse {
    pub process_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketStateEntry {
    pub process_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListenerSnapshotResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener: Option<SocketStateEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundUdpSnapshotResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket: Option<SocketStateEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalDispositionAction {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalHandlerRegistration {
    pub action: SignalDispositionAction,
    pub mask: Vec<u32>,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalStateResponse {
    pub process_id: String,
    pub handlers: BTreeMap<u32, SignalHandlerRegistration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZombieTimerCountResponse {
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemResultResponse {
    pub operation: FilesystemOperation,
    pub status: String,
    pub payload_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDecisionResponse {
    pub capability: String,
    pub decision: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceStateResponse {
    pub key: String,
    pub found: bool,
    pub payload_size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceFlushedResponse {
    pub key: String,
    pub committed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInvocationResultResponse {
    pub invocation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsBridgeResultResponse {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedResponse {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmLifecycleEvent {
    pub state: VmLifecycleState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOutputEvent {
    pub process_id: String,
    pub channel: StreamChannel,
    pub chunk: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExitedEvent {
    pub process_id: String,
    pub exit_code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredEvent {
    pub name: String,
    pub detail: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct NativeFrameCodec {
    max_frame_bytes: usize,
}

impl NativeFrameCodec {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self { max_frame_bytes }
    }

    pub fn max_frame_bytes(&self) -> usize {
        self.max_frame_bytes
    }

    pub fn encode(&self, frame: &ProtocolFrame) -> Result<Vec<u8>, ProtocolCodecError> {
        validate_frame(frame)?;

        let payload = serde_json::to_vec(frame)
            .map_err(|error| ProtocolCodecError::SerializeFailure(error.to_string()))?;
        if payload.len() > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: payload.len(),
                max: self.max_frame_bytes,
            });
        }

        let length =
            u32::try_from(payload.len()).map_err(|_| ProtocolCodecError::FrameTooLarge {
                size: payload.len(),
                max: u32::MAX as usize,
            })?;

        let mut encoded = Vec::with_capacity(4 + payload.len());
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&payload);
        Ok(encoded)
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<ProtocolFrame, ProtocolCodecError> {
        if bytes.len() < 4 {
            return Err(ProtocolCodecError::TruncatedFrame {
                actual: bytes.len(),
            });
        }

        let declared =
            u32::from_be_bytes(bytes[..4].try_into().expect("length prefix is four bytes"))
                as usize;
        if declared > self.max_frame_bytes {
            return Err(ProtocolCodecError::FrameTooLarge {
                size: declared,
                max: self.max_frame_bytes,
            });
        }

        let actual = bytes.len() - 4;
        if declared != actual {
            return Err(ProtocolCodecError::LengthPrefixMismatch { declared, actual });
        }

        let frame: ProtocolFrame = serde_json::from_slice(&bytes[4..])
            .map_err(|error| ProtocolCodecError::DeserializeFailure(error.to_string()))?;
        validate_frame(&frame)?;
        Ok(frame)
    }
}

impl Default for NativeFrameCodec {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_FRAME_BYTES)
    }
}

#[derive(Debug)]
pub struct ResponseTracker {
    pending: HashMap<RequestId, PendingRequest>,
    completed: HashSet<RequestId>,
    completed_order: VecDeque<RequestId>,
    completed_cap: usize,
}

#[derive(Debug)]
pub struct SidecarResponseTracker {
    pending: HashMap<RequestId, PendingSidecarRequest>,
    completed: HashSet<RequestId>,
    completed_order: VecDeque<RequestId>,
    completed_cap: usize,
}

impl ResponseTracker {
    pub fn with_completed_cap(completed_cap: usize) -> Self {
        Self {
            pending: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            completed_cap: completed_cap.max(1),
        }
    }

    pub fn completed_count(&self) -> usize {
        self.completed.len()
    }

    pub fn register_request(&mut self, request: &RequestFrame) -> Result<(), ResponseTrackerError> {
        if self.pending.contains_key(&request.request_id)
            || self.completed.contains(&request.request_id)
        {
            return Err(ResponseTrackerError::DuplicateRequestId {
                request_id: request.request_id,
            });
        }

        self.pending.insert(
            request.request_id,
            PendingRequest {
                ownership: request.ownership.clone(),
                expected_response: request.payload.expected_response(),
            },
        );
        Ok(())
    }

    pub fn accept_response(
        &mut self,
        response: &ResponseFrame,
    ) -> Result<(), ResponseTrackerError> {
        if self.completed.contains(&response.request_id) {
            return Err(ResponseTrackerError::DuplicateResponse {
                request_id: response.request_id,
            });
        }

        let pending = self.pending.remove(&response.request_id).ok_or(
            ResponseTrackerError::UnmatchedResponse {
                request_id: response.request_id,
            },
        )?;

        if pending.ownership != response.ownership {
            return Err(ResponseTrackerError::OwnershipMismatch {
                request_id: response.request_id,
                expected: pending.ownership,
                actual: response.ownership.clone(),
            });
        }

        if !pending.expected_response.matches(&response.payload) {
            return Err(ResponseTrackerError::ResponseKindMismatch {
                request_id: response.request_id,
                expected: pending.expected_response.as_str().to_string(),
                actual: response.payload.kind_name().to_string(),
            });
        }

        self.completed.insert(response.request_id);
        self.completed_order.push_back(response.request_id);
        while self.completed.len() > self.completed_cap {
            if let Some(evicted) = self.completed_order.pop_front() {
                self.completed.remove(&evicted);
            }
        }
        Ok(())
    }
}

impl Default for ResponseTracker {
    fn default() -> Self {
        Self::with_completed_cap(DEFAULT_COMPLETED_RESPONSE_CAP)
    }
}

impl SidecarResponseTracker {
    pub fn with_completed_cap(completed_cap: usize) -> Self {
        Self {
            pending: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            completed_cap: completed_cap.max(1),
        }
    }

    pub fn completed_count(&self) -> usize {
        self.completed.len()
    }

    pub fn register_request(
        &mut self,
        request: &SidecarRequestFrame,
    ) -> Result<(), SidecarResponseTrackerError> {
        if self.pending.contains_key(&request.request_id)
            || self.completed.contains(&request.request_id)
        {
            return Err(SidecarResponseTrackerError::DuplicateRequestId {
                request_id: request.request_id,
            });
        }

        self.pending.insert(
            request.request_id,
            PendingSidecarRequest {
                ownership: request.ownership.clone(),
                expected_response: request.payload.expected_response(),
            },
        );
        Ok(())
    }

    pub fn accept_response(
        &mut self,
        response: &SidecarResponseFrame,
    ) -> Result<(), SidecarResponseTrackerError> {
        if self.completed.contains(&response.request_id) {
            return Err(SidecarResponseTrackerError::DuplicateResponse {
                request_id: response.request_id,
            });
        }

        let pending = self.pending.remove(&response.request_id).ok_or(
            SidecarResponseTrackerError::UnmatchedResponse {
                request_id: response.request_id,
            },
        )?;

        if pending.ownership != response.ownership {
            return Err(SidecarResponseTrackerError::OwnershipMismatch {
                request_id: response.request_id,
                expected: pending.ownership,
                actual: response.ownership.clone(),
            });
        }

        if !pending.expected_response.matches(&response.payload) {
            return Err(SidecarResponseTrackerError::ResponseKindMismatch {
                request_id: response.request_id,
                expected: pending.expected_response.as_str().to_string(),
                actual: response.payload.kind_name().to_string(),
            });
        }

        self.completed.insert(response.request_id);
        self.completed_order.push_back(response.request_id);
        while self.completed.len() > self.completed_cap {
            if let Some(evicted) = self.completed_order.pop_front() {
                self.completed.remove(&evicted);
            }
        }
        Ok(())
    }
}

impl Default for SidecarResponseTracker {
    fn default() -> Self {
        Self::with_completed_cap(DEFAULT_COMPLETED_RESPONSE_CAP)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolCodecError {
    TruncatedFrame {
        actual: usize,
    },
    LengthPrefixMismatch {
        declared: usize,
        actual: usize,
    },
    FrameTooLarge {
        size: usize,
        max: usize,
    },
    UnsupportedSchema {
        name: String,
        version: u16,
    },
    InvalidRequestId,
    InvalidRequestDirection {
        request_id: RequestId,
        expected: RequestDirection,
    },
    EmptyOwnershipField {
        field: &'static str,
    },
    EmptyAuthToken,
    InvalidOwnershipScope {
        required: OwnershipRequirement,
        actual: OwnershipRequirement,
    },
    SerializeFailure(String),
    DeserializeFailure(String),
}

impl fmt::Display for ProtocolCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedFrame { actual } => {
                write!(f, "protocol frame is truncated: only {actual} bytes provided")
            }
            Self::LengthPrefixMismatch { declared, actual } => write!(
                f,
                "protocol frame length prefix mismatch: declared {declared} bytes, got {actual}",
            ),
            Self::FrameTooLarge { size, max } => {
                write!(f, "protocol frame is {size} bytes, limit is {max}")
            }
            Self::UnsupportedSchema { name, version } => write!(
                f,
                "unsupported protocol schema {name}@{version}; expected {PROTOCOL_NAME}@{PROTOCOL_VERSION}",
            ),
            Self::InvalidRequestId => write!(f, "protocol request identifiers must be non-zero"),
            Self::InvalidRequestDirection {
                request_id,
                expected,
            } => write!(
                f,
                "protocol request id {request_id} must be {expected}",
            ),
            Self::EmptyOwnershipField { field } => {
                write!(f, "protocol ownership field `{field}` cannot be empty")
            }
            Self::EmptyAuthToken => write!(f, "authenticate requests require a non-empty auth token"),
            Self::InvalidOwnershipScope { required, actual } => write!(
                f,
                "protocol frame requires {required} ownership but carried {actual}",
            ),
            Self::SerializeFailure(message) => write!(f, "protocol frame serialization failed: {message}"),
            Self::DeserializeFailure(message) => {
                write!(f, "protocol frame deserialization failed: {message}")
            }
        }
    }
}

impl Error for ProtocolCodecError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseTrackerError {
    DuplicateRequestId {
        request_id: RequestId,
    },
    UnmatchedResponse {
        request_id: RequestId,
    },
    DuplicateResponse {
        request_id: RequestId,
    },
    OwnershipMismatch {
        request_id: RequestId,
        expected: OwnershipScope,
        actual: OwnershipScope,
    },
    ResponseKindMismatch {
        request_id: RequestId,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for ResponseTrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRequestId { request_id } => {
                write!(f, "request id {request_id} is already tracked")
            }
            Self::UnmatchedResponse { request_id } => {
                write!(
                    f,
                    "response id {request_id} does not match any pending request"
                )
            }
            Self::DuplicateResponse { request_id } => {
                write!(f, "response id {request_id} has already been completed")
            }
            Self::OwnershipMismatch {
                request_id,
                expected,
                actual,
            } => write!(
                f,
                "response id {request_id} used ownership {:?}, expected {:?}",
                actual, expected
            ),
            Self::ResponseKindMismatch {
                request_id,
                expected,
                actual,
            } => write!(
                f,
                "response id {request_id} carried {actual}, expected {expected}",
            ),
        }
    }
}

impl Error for ResponseTrackerError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarResponseTrackerError {
    DuplicateRequestId {
        request_id: RequestId,
    },
    UnmatchedResponse {
        request_id: RequestId,
    },
    DuplicateResponse {
        request_id: RequestId,
    },
    OwnershipMismatch {
        request_id: RequestId,
        expected: OwnershipScope,
        actual: OwnershipScope,
    },
    ResponseKindMismatch {
        request_id: RequestId,
        expected: String,
        actual: String,
    },
}

impl fmt::Display for SidecarResponseTrackerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateRequestId { request_id } => {
                write!(f, "sidecar request id {request_id} is already tracked")
            }
            Self::UnmatchedResponse { request_id } => {
                write!(
                    f,
                    "sidecar response id {request_id} does not match any pending request"
                )
            }
            Self::DuplicateResponse { request_id } => {
                write!(
                    f,
                    "sidecar response id {request_id} has already been completed"
                )
            }
            Self::OwnershipMismatch {
                request_id,
                expected,
                actual,
            } => write!(
                f,
                "sidecar response id {request_id} used ownership {:?}, expected {:?}",
                actual, expected
            ),
            Self::ResponseKindMismatch {
                request_id,
                expected,
                actual,
            } => write!(
                f,
                "sidecar response id {request_id} carried {actual}, expected {expected}",
            ),
        }
    }
}

impl Error for SidecarResponseTrackerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipRequirement {
    Any,
    Connection,
    Session,
    Vm,
    SessionOrVm,
}

impl fmt::Display for OwnershipRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => write!(f, "any"),
            Self::Connection => write!(f, "connection"),
            Self::Session => write!(f, "session"),
            Self::Vm => write!(f, "vm"),
            Self::SessionOrVm => write!(f, "session-or-vm"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestDirection {
    Host,
    Sidecar,
}

impl fmt::Display for RequestDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => write!(f, "positive"),
            Self::Sidecar => write!(f, "negative"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingRequest {
    ownership: OwnershipScope,
    expected_response: ExpectedResponseKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingSidecarRequest {
    ownership: OwnershipScope,
    expected_response: ExpectedSidecarResponseKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedResponseKind {
    Authenticated,
    SessionOpened,
    VmCreated,
    VmDisposed,
    RootFilesystemBootstrapped,
    VmConfigured,
    GuestFilesystemResult,
    RootFilesystemSnapshot,
    ProcessStarted,
    StdinWritten,
    StdinClosed,
    ProcessKilled,
    ListenerSnapshot,
    BoundUdpSnapshot,
    SignalState,
    ZombieTimerCount,
    FilesystemResult,
    PermissionDecision,
    PersistenceState,
    PersistenceFlushed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedSidecarResponseKind {
    ToolInvocationResult,
    JsBridgeResult,
}

impl ExpectedResponseKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::SessionOpened => "session_opened",
            Self::VmCreated => "vm_created",
            Self::VmDisposed => "vm_disposed",
            Self::RootFilesystemBootstrapped => "root_filesystem_bootstrapped",
            Self::VmConfigured => "vm_configured",
            Self::GuestFilesystemResult => "guest_filesystem_result",
            Self::RootFilesystemSnapshot => "root_filesystem_snapshot",
            Self::ProcessStarted => "process_started",
            Self::StdinWritten => "stdin_written",
            Self::StdinClosed => "stdin_closed",
            Self::ProcessKilled => "process_killed",
            Self::ListenerSnapshot => "listener_snapshot",
            Self::BoundUdpSnapshot => "bound_udp_snapshot",
            Self::SignalState => "signal_state",
            Self::ZombieTimerCount => "zombie_timer_count",
            Self::FilesystemResult => "filesystem_result",
            Self::PermissionDecision => "permission_decision",
            Self::PersistenceState => "persistence_state",
            Self::PersistenceFlushed => "persistence_flushed",
        }
    }

    fn matches(self, payload: &ResponsePayload) -> bool {
        match payload {
            ResponsePayload::Rejected(_) => true,
            _ => payload.kind_name() == self.as_str(),
        }
    }
}

impl ExpectedSidecarResponseKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::ToolInvocationResult => "tool_invocation_result",
            Self::JsBridgeResult => "js_bridge_result",
        }
    }

    fn matches(self, payload: &SidecarResponsePayload) -> bool {
        payload.kind_name() == self.as_str()
    }
}

impl RequestPayload {
    fn ownership_requirement(&self) -> OwnershipRequirement {
        match self {
            Self::Authenticate(_) | Self::OpenSession(_) => OwnershipRequirement::Connection,
            Self::CreateVm(_) | Self::PersistenceLoad(_) | Self::PersistenceFlush(_) => {
                OwnershipRequirement::Session
            }
            Self::DisposeVm(_)
            | Self::BootstrapRootFilesystem(_)
            | Self::ConfigureVm(_)
            | Self::GuestFilesystemCall(_)
            | Self::SnapshotRootFilesystem(_)
            | Self::Execute(_)
            | Self::WriteStdin(_)
            | Self::CloseStdin(_)
            | Self::KillProcess(_)
            | Self::FindListener(_)
            | Self::FindBoundUdp(_)
            | Self::GetSignalState(_)
            | Self::GetZombieTimerCount(_)
            | Self::HostFilesystemCall(_)
            | Self::PermissionRequest(_) => OwnershipRequirement::Vm,
        }
    }

    fn expected_response(&self) -> ExpectedResponseKind {
        match self {
            Self::Authenticate(_) => ExpectedResponseKind::Authenticated,
            Self::OpenSession(_) => ExpectedResponseKind::SessionOpened,
            Self::CreateVm(_) => ExpectedResponseKind::VmCreated,
            Self::DisposeVm(_) => ExpectedResponseKind::VmDisposed,
            Self::BootstrapRootFilesystem(_) => ExpectedResponseKind::RootFilesystemBootstrapped,
            Self::ConfigureVm(_) => ExpectedResponseKind::VmConfigured,
            Self::GuestFilesystemCall(_) => ExpectedResponseKind::GuestFilesystemResult,
            Self::SnapshotRootFilesystem(_) => ExpectedResponseKind::RootFilesystemSnapshot,
            Self::Execute(_) => ExpectedResponseKind::ProcessStarted,
            Self::WriteStdin(_) => ExpectedResponseKind::StdinWritten,
            Self::CloseStdin(_) => ExpectedResponseKind::StdinClosed,
            Self::KillProcess(_) => ExpectedResponseKind::ProcessKilled,
            Self::FindListener(_) => ExpectedResponseKind::ListenerSnapshot,
            Self::FindBoundUdp(_) => ExpectedResponseKind::BoundUdpSnapshot,
            Self::GetSignalState(_) => ExpectedResponseKind::SignalState,
            Self::GetZombieTimerCount(_) => ExpectedResponseKind::ZombieTimerCount,
            Self::HostFilesystemCall(_) => ExpectedResponseKind::FilesystemResult,
            Self::PermissionRequest(_) => ExpectedResponseKind::PermissionDecision,
            Self::PersistenceLoad(_) => ExpectedResponseKind::PersistenceState,
            Self::PersistenceFlush(_) => ExpectedResponseKind::PersistenceFlushed,
        }
    }
}

impl SidecarRequestPayload {
    fn ownership_requirement(&self) -> OwnershipRequirement {
        OwnershipRequirement::Vm
    }

    fn expected_response(&self) -> ExpectedSidecarResponseKind {
        match self {
            Self::ToolInvocation(_) => ExpectedSidecarResponseKind::ToolInvocationResult,
            Self::JsBridgeCall(_) => ExpectedSidecarResponseKind::JsBridgeResult,
        }
    }
}

impl ResponsePayload {
    fn ownership_requirement(&self) -> OwnershipRequirement {
        match self {
            Self::Authenticated(_) | Self::SessionOpened(_) => OwnershipRequirement::Connection,
            Self::VmCreated(_) | Self::PersistenceState(_) | Self::PersistenceFlushed(_) => {
                OwnershipRequirement::Session
            }
            Self::Rejected(_) => OwnershipRequirement::Any,
            Self::VmDisposed(_)
            | Self::RootFilesystemBootstrapped(_)
            | Self::VmConfigured(_)
            | Self::GuestFilesystemResult(_)
            | Self::RootFilesystemSnapshot(_)
            | Self::ProcessStarted(_)
            | Self::StdinWritten(_)
            | Self::StdinClosed(_)
            | Self::ProcessKilled(_)
            | Self::ListenerSnapshot(_)
            | Self::BoundUdpSnapshot(_)
            | Self::SignalState(_)
            | Self::ZombieTimerCount(_)
            | Self::FilesystemResult(_)
            | Self::PermissionDecision(_) => OwnershipRequirement::Vm,
        }
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Self::Authenticated(_) => "authenticated",
            Self::SessionOpened(_) => "session_opened",
            Self::VmCreated(_) => "vm_created",
            Self::VmDisposed(_) => "vm_disposed",
            Self::RootFilesystemBootstrapped(_) => "root_filesystem_bootstrapped",
            Self::VmConfigured(_) => "vm_configured",
            Self::GuestFilesystemResult(_) => "guest_filesystem_result",
            Self::RootFilesystemSnapshot(_) => "root_filesystem_snapshot",
            Self::ProcessStarted(_) => "process_started",
            Self::StdinWritten(_) => "stdin_written",
            Self::StdinClosed(_) => "stdin_closed",
            Self::ProcessKilled(_) => "process_killed",
            Self::ListenerSnapshot(_) => "listener_snapshot",
            Self::BoundUdpSnapshot(_) => "bound_udp_snapshot",
            Self::SignalState(_) => "signal_state",
            Self::ZombieTimerCount(_) => "zombie_timer_count",
            Self::FilesystemResult(_) => "filesystem_result",
            Self::PermissionDecision(_) => "permission_decision",
            Self::PersistenceState(_) => "persistence_state",
            Self::PersistenceFlushed(_) => "persistence_flushed",
            Self::Rejected(_) => "rejected",
        }
    }
}

impl SidecarResponsePayload {
    fn ownership_requirement(&self) -> OwnershipRequirement {
        OwnershipRequirement::Vm
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Self::ToolInvocationResult(_) => "tool_invocation_result",
            Self::JsBridgeResult(_) => "js_bridge_result",
        }
    }
}

impl EventPayload {
    fn ownership_requirement(&self) -> OwnershipRequirement {
        match self {
            Self::Structured(_) => OwnershipRequirement::SessionOrVm,
            Self::VmLifecycle(_) | Self::ProcessOutput(_) | Self::ProcessExited(_) => {
                OwnershipRequirement::Vm
            }
        }
    }
}

pub fn validate_frame(frame: &ProtocolFrame) -> Result<(), ProtocolCodecError> {
    match frame {
        ProtocolFrame::Request(request) => validate_request(request),
        ProtocolFrame::Response(response) => validate_response(response),
        ProtocolFrame::Event(event) => validate_event(event),
        ProtocolFrame::SidecarRequest(request) => validate_sidecar_request(request),
        ProtocolFrame::SidecarResponse(response) => validate_sidecar_response(response),
    }
}

fn validate_request(request: &RequestFrame) -> Result<(), ProtocolCodecError> {
    validate_schema(&request.schema)?;
    validate_request_id_direction(request.request_id, RequestDirection::Host)?;

    validate_ownership(&request.ownership)?;
    validate_requirement(request.payload.ownership_requirement(), &request.ownership)?;
    if let RequestPayload::Authenticate(authenticate) = &request.payload {
        if authenticate.auth_token.is_empty() {
            return Err(ProtocolCodecError::EmptyAuthToken);
        }
    }

    Ok(())
}

fn validate_response(response: &ResponseFrame) -> Result<(), ProtocolCodecError> {
    validate_schema(&response.schema)?;
    validate_request_id_direction(response.request_id, RequestDirection::Host)?;

    validate_ownership(&response.ownership)?;
    validate_requirement(
        response.payload.ownership_requirement(),
        &response.ownership,
    )?;
    Ok(())
}

fn validate_sidecar_request(request: &SidecarRequestFrame) -> Result<(), ProtocolCodecError> {
    validate_schema(&request.schema)?;
    validate_request_id_direction(request.request_id, RequestDirection::Sidecar)?;
    validate_ownership(&request.ownership)?;
    validate_requirement(request.payload.ownership_requirement(), &request.ownership)?;
    Ok(())
}

fn validate_sidecar_response(response: &SidecarResponseFrame) -> Result<(), ProtocolCodecError> {
    validate_schema(&response.schema)?;
    validate_request_id_direction(response.request_id, RequestDirection::Sidecar)?;
    validate_ownership(&response.ownership)?;
    validate_requirement(
        response.payload.ownership_requirement(),
        &response.ownership,
    )?;
    Ok(())
}

fn validate_event(event: &EventFrame) -> Result<(), ProtocolCodecError> {
    validate_schema(&event.schema)?;
    validate_ownership(&event.ownership)?;
    validate_requirement(event.payload.ownership_requirement(), &event.ownership)?;
    Ok(())
}

fn validate_schema(schema: &ProtocolSchema) -> Result<(), ProtocolCodecError> {
    if schema.name != PROTOCOL_NAME || schema.version != PROTOCOL_VERSION {
        return Err(ProtocolCodecError::UnsupportedSchema {
            name: schema.name.clone(),
            version: schema.version,
        });
    }

    Ok(())
}

fn validate_ownership(ownership: &OwnershipScope) -> Result<(), ProtocolCodecError> {
    match ownership {
        OwnershipScope::Connection { connection_id } => {
            validate_non_empty("connection_id", connection_id)
        }
        OwnershipScope::Session {
            connection_id,
            session_id,
        } => {
            validate_non_empty("connection_id", connection_id)?;
            validate_non_empty("session_id", session_id)
        }
        OwnershipScope::Vm {
            connection_id,
            session_id,
            vm_id,
        } => {
            validate_non_empty("connection_id", connection_id)?;
            validate_non_empty("session_id", session_id)?;
            validate_non_empty("vm_id", vm_id)
        }
    }
}

fn validate_non_empty(field: &'static str, value: &str) -> Result<(), ProtocolCodecError> {
    if value.is_empty() {
        return Err(ProtocolCodecError::EmptyOwnershipField { field });
    }

    Ok(())
}

fn validate_request_id_direction(
    request_id: RequestId,
    direction: RequestDirection,
) -> Result<(), ProtocolCodecError> {
    if request_id == 0 {
        return Err(ProtocolCodecError::InvalidRequestId);
    }

    let matches_direction = match direction {
        RequestDirection::Host => request_id > 0,
        RequestDirection::Sidecar => request_id < 0,
    };
    if matches_direction {
        Ok(())
    } else {
        Err(ProtocolCodecError::InvalidRequestDirection {
            request_id,
            expected: direction,
        })
    }
}

fn validate_requirement(
    required: OwnershipRequirement,
    ownership: &OwnershipScope,
) -> Result<(), ProtocolCodecError> {
    let actual = match ownership {
        OwnershipScope::Connection { .. } => OwnershipRequirement::Connection,
        OwnershipScope::Session { .. } => OwnershipRequirement::Session,
        OwnershipScope::Vm { .. } => OwnershipRequirement::Vm,
    };

    let valid = match required {
        OwnershipRequirement::Any => true,
        OwnershipRequirement::Connection => matches!(ownership, OwnershipScope::Connection { .. }),
        OwnershipRequirement::Session => matches!(ownership, OwnershipScope::Session { .. }),
        OwnershipRequirement::Vm => matches!(ownership, OwnershipScope::Vm { .. }),
        OwnershipRequirement::SessionOrVm => {
            matches!(
                ownership,
                OwnershipScope::Session { .. } | OwnershipScope::Vm { .. }
            )
        }
    };

    if valid {
        Ok(())
    } else {
        Err(ProtocolCodecError::InvalidOwnershipScope { required, actual })
    }
}

// ---------------------------------------------------------------------------
// JavaScript sync-RPC request types (deserialized from guest Node.js processes)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct JavascriptChildProcessSpawnOptions {
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(rename = "internalBootstrapEnv", default)]
    pub internal_bootstrap_env: BTreeMap<String, String>,
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptChildProcessSpawnRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub options: JavascriptChildProcessSpawnOptions,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptNetConnectRequest {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptNetListenRequest {
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub backlog: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptDgramCreateSocketRequest {
    #[serde(rename = "type")]
    pub socket_type: String,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptDgramBindRequest {
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptDgramSendRequest {
    #[serde(default)]
    pub address: Option<String>,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptDnsLookupRequest {
    pub hostname: String,
    #[serde(default)]
    pub family: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub struct JavascriptDnsResolveRequest {
    pub hostname: String,
    #[serde(default)]
    pub rrtype: Option<String>,
}
