#![forbid(unsafe_code)]

//! # agentos-client
//!
//! High-level Rust client SDK for the Agent OS native sidecar. This is a 1:1 port of the TypeScript
//! `AgentOs` client (`packages/core/src/agent-os.ts`): every public method, option type, return
//! type, event, and error maps across with identical semantics.
//!
//! The client spawns the native `agentos-sidecar` binary and speaks the existing framed BARE
//! protocol over its stdio (see [`transport`]). It does NOT embed the kernel in-process and does NOT
//! define a new sidecar wire protocol. The generated AgentOS language execution schema surface comes from
//! `agentos_sidecar_client::wire`; Agent OS layers ACP/session semantics on top of those generated wire
//! frames through the wrapper client.
//!
//! See the companion design docs in `~/.agents/specs/rust-client-sdk/` (ADR-001, spec, reference,
//! checklist) for the architecture, type-mapping, error taxonomy, and streaming model.

pub mod agent_os;
pub(crate) mod command_line;
pub mod config;
pub mod cron;
pub mod error;
pub mod fs;
pub mod language_execution;
pub mod net;
pub mod process;
pub mod session;
pub mod sidecar;
pub mod stream;
pub mod transport;

// ---------------------------------------------------------------------------
// Centralized constants (ADR-001 §6 / spec.md §7)
// ---------------------------------------------------------------------------

/// ACP protocol version negotiated on session creation.
pub const ACP_PROTOCOL_VERSION: u64 = 1;

/// Bounded exited-shell exit-code retention (for `wait_shell` after exit).
pub const CLOSED_SHELL_EXIT_CODE_RETENTION_LIMIT: usize = 2048;

/// Two-phase shell-drain timeout during dispose (milliseconds).
pub const SHELL_DISPOSE_TIMEOUT_MS: u64 = 5_000;

/// VM lifecycle ready timeout during `create` (milliseconds).
pub const VM_READY_TIMEOUT_MS: u64 = 10_000;

/// Maximum scheduled cron jobs per VM.
pub const CRON_JOB_LIMIT: usize = 1024;

// ---------------------------------------------------------------------------
// Public re-exports
// ---------------------------------------------------------------------------

pub use agent_os::{AgentOs, PackageDescriptor, ProjectedAgent, SoftwareInfo};
pub use error::{ClientError, ClientResult, ResourceLimitDetails};
pub use language_execution::{
    CodeEvaluationResult, CodeExecutionResult, ContextDescriptor, ExecutionPtyOptions,
    InlineExecutionOptions, JavaScriptExecutionOptions, JavaScriptModuleFormat,
    LanguageExecutionOptions, LanguageSpawnOptions, ProcessDescriptor, TypeScriptDiagnostic,
};
pub use sidecar::{
    AgentOsSidecar, AgentOsSidecarDescription, AgentOsSidecarPlacement, SidecarState,
};
pub use stream::{ByteStream, Subscription};

pub use config::{
    node_modules_mount, AcpLimits, AgentOsConfig, AgentOsConfigBuilder, AgentOsLimits,
    AgentOsSidecarConfig, Binding, BindingCallback, BindingLimits, Bindings, FsPermissionRule,
    FsPermissions, HttpLimits, JsRuntimeLimits, MountConfig, MountPlugin, OverlayMountConfig,
    PackageRef, PatternPermissionRule, PatternPermissions, PermissionMode, Permissions,
    PluginLimits, PythonLimits, ResourceLimits, RootFilesystemConfig, RootFilesystemKind,
    RootFilesystemMode, RootLowerInput, RulePermissions, ScheduleCallback, ScheduleDriver,
    ScheduleEntry, ScheduleHandle, SidecarJsBridgeCall, SidecarJsBridgeCallback, SoftwareInput,
    SoftwareKind, TimerScheduleDriver, VmGroupConfig, VmUserAccountConfig, VmUserConfig,
    WasmLimits,
};

pub use process::{
    ExecOptions, ExecResult, ProcessExit, ProcessInfo, ProcessOutput, ProcessStatus, ProcessStream,
    ProcessTreeNode, SpawnHandle, SpawnOptions, SpawnStdio, SpawnedProcessInfo, StdinInput,
    TimingMitigation,
};

pub use net::{HttpRequest, HttpResponse};

pub use fs::{
    BatchReadResult, BatchWriteEntry, BatchWriteResult, DirEntry, DirEntryType,
    DynamicMountDescriptor, FileContent, FilesystemEntry, FilesystemEntryEncoding,
    FilesystemSnapshotEntries, FilesystemSnapshotExport, MkdirOptions, MountInfo,
    ReaddirRecursiveOptions, RemoveOptions, RootSnapshotExport, SnapshotExportKind,
    VirtualDirEntry, VirtualFileSystem, VirtualStat,
};

pub use shell::{ConnectTerminalOptions, OpenShellOptions, ShellData, ShellExit, ShellHandle};

pub use session::{
    AgentExitEvent, AgentExitStream, AgentExitSubscription, AgentMessage, AgentRegistryEntry,
    AgentRestartOutcome, CancelPromptStatus, ContentBlock, DurableEventKind, DurableSessionEvent,
    DurableSessionEventEntry, DurableSessionEventStream, DurableSessionEventSubscription,
    EphemeralEventKind, EphemeralSessionEvent, EphemeralSessionEventEntry, HistoryPage,
    ListSessionsInput, McpServerConfig, OpenSessionInput, PendingPermissionRequest,
    PermissionEventStatus, PermissionPolicy, PermissionResponseStatus, PermissionTerminalReason,
    PromptInput, PromptResult, ReadHistoryInput, SessionCapabilities, SessionConfig,
    SessionConfigOption, SessionConfigValue, SessionInfo, SessionPage, SessionState,
    SessionStreamEntry, SessionSubscriptionError, SessionUpdate, StopReason,
};

pub use cron::{
    CronAction, CronActionInfo, CronEvent, CronJobHandle, CronJobInfo, CronJobOptions, CronManager,
    CronOverlap, CronSessionOptions,
};

// `shell` is declared here because its methods live in a sibling module to keep `lib.rs` re-exports
// flat; the module file itself is `shell.rs`.
pub mod shell;
