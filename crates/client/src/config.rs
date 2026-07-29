//! Configuration types: `AgentOsConfig` (= TS `AgentOsOptions`), the permissions tree, root
//! filesystem config, mount config, and the schedule-driver abstraction.
//!
//! Ported from `packages/core/src/agent-os.ts` (`AgentOsOptions`), `runtime.ts` (`Permissions`),
//! `layers.ts` / `overlay-filesystem.ts` (root/overlay), and `cron/` (schedule driver).
//!
//! Non-serializable parameters (`MountConfig::Plain.driver`, `CronAction::Callback`) are in-process
//! only and become `Arc<dyn ...>` trait objects; they cannot cross the wire and are gated exactly as
//! the actor layer gates them.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::fs::VirtualFileSystem;
pub use agentos_vm_config::{VmGroupConfig, VmUserAccountConfig, VmUserConfig};

/// Resolved client options (= TS `AgentOsOptions`). All fields optional with documented defaults.
///
/// Keep this Rust mirror in sync with `packages/core/src/agent-os.ts::AgentOsOptions`
/// and `packages/core/src/options-schema.ts::agentOsOptionsSchema`.
#[derive(Default)]
pub struct AgentOsConfig {
    /// VM-scoped SQLite backend shared by VFS metadata/storage and AgentOS
    /// durable state. Actor deployments inject `ActorUds`; standalone clients
    /// normally provide a `SqliteFile` descriptor.
    pub database: Option<agentos_vm_config::VmSqliteDescriptor>,
    /// Initial virtual Linux credentials and account record. Defaults to `1000:1000` (`agentos`).
    pub user: Option<VmUserConfig>,
    /// Software packages to install (flattened). Default `[]`.
    pub software: Vec<SoftwareInput>,
    /// Package directories to project into the VM's `/opt/agentos` tree (the
    /// agentos package projection). Each entry is a host dir containing an
    /// `agentos-package.json` manifest + the package payload. Default `[]`.
    pub packages: Vec<PackageRef>,
    /// Guest mount point for the package projection. Default `/opt/agentos`
    /// (agentos's `OPT_AGENTOS_ROOT`) when `None`.
    pub packages_mount_at: Option<String>,
    /// Loopback ports exempt from the default outbound-to-host block.
    pub loopback_exempt_ports: Vec<u16>,
    /// Allowed Node.js builtins. Default: the hardened native-bridge set.
    pub allowed_node_builtins: Option<Vec<String>>,
    /// Root filesystem configuration. Default: overlay + bundled base snapshot.
    pub root_filesystem: RootFilesystemConfig,
    /// Additional mounts.
    pub mounts: Vec<MountConfig>,
    /// Extra OS instructions appended to agent sessions.
    pub additional_instructions: Option<String>,
    /// Schedule driver used by the cron manager. Default: [`TimerScheduleDriver`].
    pub schedule_driver: Option<Arc<dyn ScheduleDriver>>,
    /// Binding collections to register.
    pub bindings: Vec<Bindings>,
    /// Rust-only sidecar callback handler for `js_bridge`-style plugin requests.
    pub sidecar_js_bridge_callback: Option<SidecarJsBridgeCallback>,
    /// Permission policy. Default: allow-all.
    pub permissions: Option<Permissions>,
    /// Operator-tunable VM limits. Default: sidecar/kernel built-ins.
    pub limits: Option<AgentOsLimits>,
    /// Sidecar placement/config. Default: shared `default` pool.
    pub sidecar: Option<AgentOsSidecarConfig>,
    /// Absolute path to the `agentos-sidecar` binary, resolved from the npm
    /// package on the TypeScript side. Threaded to `SidecarProcess::spawn`
    /// (mirroring rivetkit's `engine_binary_path`) instead of relying on the
    /// `AGENTOS_SIDECAR_BIN` env var. `None` falls back to env, then `PATH`.
    pub sidecar_binary_path: Option<String>,
}

/// Builder for [`AgentOsConfig`].
#[derive(Default)]
pub struct AgentOsConfigBuilder {
    config: AgentOsConfig,
}

impl AgentOsConfigBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn database(mut self, database: agentos_vm_config::VmSqliteDescriptor) -> Self {
        self.config.database = Some(database);
        self
    }

    pub fn packages(mut self, packages: Vec<PackageRef>) -> Self {
        self.config.packages = packages;
        self
    }

    pub fn packages_mount_at(mut self, mount_at: impl Into<String>) -> Self {
        self.config.packages_mount_at = Some(mount_at.into());
        self
    }

    pub fn loopback_exempt_ports(mut self, ports: Vec<u16>) -> Self {
        self.config.loopback_exempt_ports = ports;
        self
    }

    pub fn allowed_node_builtins(mut self, builtins: Vec<String>) -> Self {
        self.config.allowed_node_builtins = Some(builtins);
        self
    }

    pub fn user(mut self, user: VmUserConfig) -> Self {
        self.config.user = Some(user);
        self
    }

    pub fn root_filesystem(mut self, root: RootFilesystemConfig) -> Self {
        self.config.root_filesystem = root;
        self
    }

    pub fn mounts(mut self, mounts: Vec<MountConfig>) -> Self {
        self.config.mounts = mounts;
        self
    }

    pub fn additional_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.config.additional_instructions = Some(instructions.into());
        self
    }

    pub fn schedule_driver(mut self, driver: Arc<dyn ScheduleDriver>) -> Self {
        self.config.schedule_driver = Some(driver);
        self
    }

    pub fn bindings(mut self, bindings: Vec<Bindings>) -> Self {
        self.config.bindings = bindings;
        self
    }

    pub fn sidecar_js_bridge_callback(mut self, callback: SidecarJsBridgeCallback) -> Self {
        self.config.sidecar_js_bridge_callback = Some(callback);
        self
    }

    pub fn permissions(mut self, permissions: Permissions) -> Self {
        self.config.permissions = Some(permissions);
        self
    }

    pub fn limits(mut self, limits: AgentOsLimits) -> Self {
        self.config.limits = Some(limits);
        self
    }

    pub fn sidecar(mut self, sidecar: AgentOsSidecarConfig) -> Self {
        self.config.sidecar = Some(sidecar);
        self
    }

    pub fn sidecar_binary_path(mut self, path: impl Into<String>) -> Self {
        self.config.sidecar_binary_path = Some(path.into());
        self
    }

    pub fn build(self) -> AgentOsConfig {
        self.config
    }
}

/// The kind of a software package, which decides how it is mounted into the VM. Mirrors the TS
/// descriptor `type` discriminator (`packages/core/src/packages.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SoftwareKind {
    /// A directory of wasm command binaries. Mounted at `/__agentos/commands/{index}/` so the
    /// sidecar's command discovery can resolve guest commands (`echo`, `sh`, `grep`, ...).
    #[default]
    WasmCommands,
    /// An agent SDK/adapter package. Not mounted as a command directory.
    Agent,
    /// A host-binding package. Not mounted as a command directory.
    Binding,
}

/// A flattened software package input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoftwareInput {
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// How the package is mounted into the VM. Defaults to [`SoftwareKind::WasmCommands`].
    #[serde(default)]
    pub kind: SoftwareKind,
}

/// A reference to a packed `.aospkg` package for the `/opt/agentos`
/// projection. A directory path remains accepted for local transition
/// fixtures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRef {
    /// Serialized as `packagePath` — the single package-ref spelling on every
    /// JSON boundary (registry exports, actor config, client config).
    #[serde(rename = "packagePath")]
    pub path: String,
}

/// A host-side binding execute callback. Receives the validated JSON input, returns a JSON result or an
/// error string. Stays host-side (never crosses to the guest); the guest invokes it by name via the
/// sidecar host-callback channel.
pub type BindingCallback = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> futures::future::BoxFuture<'static, Result<serde_json::Value, String>>
        + Send
        + Sync,
>;

/// A sidecar-initiated `js_bridge`-style filesystem callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarJsBridgeCall {
    pub call_id: String,
    pub mount_id: String,
    pub operation: String,
    pub args: serde_json::Value,
}

/// Host-side handler for sidecar `JsBridgeCallRequest` payloads.
///
/// This is Rust-only and intentionally not JSON-serializable. RivetKit uses it to bind a native
/// sidecar root filesystem to actor-owned SQLite (`ctx.db_*`) without teaching agentos about
/// Rivet actors.
pub type SidecarJsBridgeCallback = Arc<
    dyn Fn(
            SidecarJsBridgeCall,
        )
            -> futures::future::BoxFuture<'static, Result<Option<serde_json::Value>, String>>
        + Send
        + Sync,
>;

/// A single host binding within a [`Bindings`].
#[derive(Clone)]
pub struct Binding {
    pub name: String,
    pub description: String,
    /// JSON Schema for the binding input (forwarded to the sidecar `register_host_callbacks` definition).
    pub input_schema: serde_json::Value,
    pub timeout_ms: Option<u64>,
    /// Host-side implementation, invoked when the guest calls `<collection>:<binding>`.
    pub execute: BindingCallback,
}

/// A registered binding collection (in-process; implementations stay host-side). Bindings are exposed to the
/// guest as `<collection>:<binding>` and dispatched back to [`Binding::execute`] via the sidecar
/// host-callback channel.
#[derive(Clone)]
pub struct Bindings {
    pub name: String,
    pub description: String,
    pub bindings: Vec<Binding>,
}

// ---------------------------------------------------------------------------
// VM limits (agent-os.ts AgentOsLimits / sidecar/limits.ts)
// ---------------------------------------------------------------------------

/// Operator-tunable runtime limits for a VM. Every field is optional; unset fields fall back to the
/// sidecar defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOsLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<BindingLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<PluginLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp: Option<AcpLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlite: Option<SqliteLimits>,
    #[serde(default, rename = "jsRuntime", skip_serializing_if = "Option::is_none")]
    pub js_runtime: Option<JsRuntimeLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<PythonLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm: Option<WasmLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessLimits>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    #[serde(default, rename = "cpuCount", skip_serializing_if = "Option::is_none")]
    pub cpu_count: Option<u64>,
    #[serde(
        default,
        rename = "maxProcesses",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_processes: Option<u64>,
    #[serde(
        default,
        rename = "maxOpenFds",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_open_fds: Option<u64>,
    #[serde(default, rename = "maxPipes", skip_serializing_if = "Option::is_none")]
    pub max_pipes: Option<u64>,
    #[serde(default, rename = "maxPtys", skip_serializing_if = "Option::is_none")]
    pub max_ptys: Option<u64>,
    #[serde(
        default,
        rename = "maxSockets",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_sockets: Option<u64>,
    #[serde(
        default,
        rename = "maxConnections",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_connections: Option<u64>,
    #[serde(
        default,
        rename = "maxSocketBufferedBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_socket_buffered_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxSocketDatagramQueueLen",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_socket_datagram_queue_len: Option<u64>,
    #[serde(
        default,
        rename = "maxFilesystemBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_filesystem_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxInodeCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_inode_count: Option<u64>,
    #[serde(
        default,
        rename = "maxBlockingReadMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_blocking_read_ms: Option<u64>,
    #[serde(
        default,
        rename = "maxPreadBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_pread_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxFdWriteBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_fd_write_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxProcessArgvBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_process_argv_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxProcessEnvBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_process_env_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxReaddirEntries",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_readdir_entries: Option<u64>,
    #[serde(
        default,
        rename = "maxWasmFuel",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_wasm_fuel: Option<u64>,
    #[serde(
        default,
        rename = "maxWasmMemoryBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_wasm_memory_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxWasmStackBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_wasm_stack_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpLimits {
    #[serde(
        default,
        rename = "maxFetchResponseBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_fetch_response_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingLimits {
    #[serde(
        default,
        rename = "defaultBindingTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub default_binding_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "maxBindingTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_binding_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "maxRegisteredCollections",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_registered_collections: Option<u64>,
    #[serde(
        default,
        rename = "maxRegisteredBindingsPerVm",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_registered_bindings_per_vm: Option<u64>,
    #[serde(
        default,
        rename = "maxBindingsPerCollection",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_bindings_per_collection: Option<u64>,
    #[serde(
        default,
        rename = "maxBindingSchemaBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_binding_schema_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxExamplesPerBinding",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_examples_per_binding: Option<u64>,
    #[serde(
        default,
        rename = "maxBindingExampleInputBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_binding_example_input_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLimits {
    #[serde(
        default,
        rename = "maxPersistedManifestBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_persisted_manifest_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxPersistedManifestFileBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_persisted_manifest_file_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpLimits {
    #[serde(
        default,
        rename = "maxReadLineBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_read_line_bytes: Option<u64>,
    #[serde(
        default,
        rename = "stdoutBufferByteLimit",
        skip_serializing_if = "Option::is_none"
    )]
    pub stdout_buffer_byte_limit: Option<u64>,
    #[serde(
        default,
        rename = "maxCompletedMessageBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_completed_message_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxTurnOutputBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_turn_output_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxPromptBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_prompt_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxPromptBlocks",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_prompt_blocks: Option<u64>,
    #[serde(
        default,
        rename = "maxFallbackContinuationBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_fallback_continuation_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxSessionHistoryBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_session_history_bytes: Option<u64>,
    #[serde(
        default,
        rename = "maxSessionHistoryEvents",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_session_history_events: Option<u64>,
    #[serde(
        default,
        rename = "maxHistoryPageEntries",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_history_page_entries: Option<u64>,
    #[serde(
        default,
        rename = "maxSessionListEntries",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_session_list_entries: Option<u64>,
    #[serde(
        default,
        rename = "maxSessionsPerVm",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_sessions_per_vm: Option<u64>,
    #[serde(
        default,
        rename = "maxPromptsPerSession",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_prompts_per_session: Option<u64>,
    #[serde(
        default,
        rename = "maxPromptsPerVm",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_prompts_per_vm: Option<u64>,
    #[serde(
        default,
        rename = "maxPendingPermissionsPerSession",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_pending_permissions_per_session: Option<u64>,
    #[serde(
        default,
        rename = "maxPendingPermissionsPerVm",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_pending_permissions_per_vm: Option<u64>,
    #[serde(
        default,
        rename = "maxPermissionOutcomesPerSession",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_permission_outcomes_per_session: Option<u64>,
    #[serde(
        default,
        rename = "maxPermissionOutcomesPerVm",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_permission_outcomes_per_vm: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SqliteLimits {
    #[serde(
        default,
        rename = "maxResultBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_result_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsRuntimeLimits {
    #[serde(
        default,
        rename = "v8HeapLimitMb",
        skip_serializing_if = "Option::is_none"
    )]
    pub v8_heap_limit_mb: Option<u64>,
    #[serde(
        default,
        rename = "syncRpcWaitTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub sync_rpc_wait_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "cpuTimeLimitMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub cpu_time_limit_ms: Option<u64>,
    #[serde(
        default,
        rename = "wallClockLimitMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub wall_clock_limit_ms: Option<u64>,
    #[serde(
        default,
        rename = "importCacheMaterializeTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub import_cache_materialize_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "capturedOutputLimitBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub captured_output_limit_bytes: Option<u64>,
    #[serde(
        default,
        rename = "stdinBufferLimitBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub stdin_buffer_limit_bytes: Option<u64>,
    #[serde(
        default,
        rename = "eventPayloadLimitBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub event_payload_limit_bytes: Option<u64>,
    #[serde(
        default,
        rename = "v8IpcMaxFrameBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub v8_ipc_max_frame_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonLimits {
    #[serde(
        default,
        rename = "outputBufferMaxBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub output_buffer_max_bytes: Option<u64>,
    #[serde(
        default,
        rename = "executionTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub execution_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "maxOldSpaceMb",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_old_space_mb: Option<u64>,
    #[serde(
        default,
        rename = "vfsRpcTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub vfs_rpc_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmLimits {
    #[serde(
        default,
        rename = "maxModuleFileBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_module_file_bytes: Option<u64>,
    #[serde(
        default,
        rename = "capturedOutputLimitBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub captured_output_limit_bytes: Option<u64>,
    #[serde(
        default,
        rename = "syncReadLimitBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub sync_read_limit_bytes: Option<u64>,
    #[serde(
        default,
        rename = "prewarmTimeoutMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub prewarm_timeout_ms: Option<u64>,
    #[serde(
        default,
        rename = "runnerHeapLimitMb",
        skip_serializing_if = "Option::is_none"
    )]
    pub runner_heap_limit_mb: Option<u64>,
    #[serde(
        default,
        rename = "runnerCpuTimeLimitMs",
        skip_serializing_if = "Option::is_none"
    )]
    pub runner_cpu_time_limit_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessLimits {
    #[serde(
        default,
        rename = "maxSpawnFileActions",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_spawn_file_actions: Option<u64>,
    #[serde(
        default,
        rename = "maxSpawnFileActionBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_spawn_file_action_bytes: Option<u64>,
    #[serde(
        default,
        rename = "pendingStdinBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_stdin_bytes: Option<u64>,
    #[serde(
        default,
        rename = "pendingEventCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_event_count: Option<u64>,
    #[serde(
        default,
        rename = "pendingEventBytes",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_event_bytes: Option<u64>,
}

// ---------------------------------------------------------------------------
// Permissions tree (runtime.ts)
// ---------------------------------------------------------------------------

/// Top-level permission policy. All domains optional (`allowAll` when omitted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<FsPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<PatternPermissions>,
    #[serde(
        default,
        rename = "childProcess",
        skip_serializing_if = "Option::is_none"
    )]
    pub child_process: Option<PatternPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<PatternPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<PatternPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<PatternPermissions>,
}

/// `"allow"` or `"deny"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionMode {
    Allow,
    Deny,
}

/// `PermissionMode | RulePermissions<FsPermissionRule>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FsPermissions {
    Mode(PermissionMode),
    Rules(RulePermissions<FsPermissionRule>),
}

/// `PermissionMode | RulePermissions<PatternPermissionRule>` (network/childProcess/process/env/binding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PatternPermissions {
    Mode(PermissionMode),
    Rules(RulePermissions<PatternPermissionRule>),
}

/// `{ default?: PermissionMode; rules: T[] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulePermissions<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PermissionMode>,
    pub rules: Vec<T>,
}

/// `{ mode; operations?; paths? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsPermissionRule {
    pub mode: PermissionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

/// `{ mode; operations?; patterns? }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternPermissionRule {
    pub mode: PermissionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operations: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patterns: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Root filesystem (layers.ts / overlay-filesystem.ts)
// ---------------------------------------------------------------------------

/// Root filesystem configuration. Default: overlay + bundled base snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootFilesystemConfig {
    #[serde(default, rename = "type")]
    pub kind: RootFilesystemKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RootFilesystemMode>,
    #[serde(
        default,
        rename = "nativePlugin",
        skip_serializing_if = "Option::is_none"
    )]
    pub native_plugin: Option<MountPlugin>,
    #[serde(default, rename = "disableDefaultBaseLayer")]
    pub disable_default_base_layer: bool,
    #[serde(default)]
    pub lowers: Vec<RootLowerInput>,
}

impl Default for RootFilesystemConfig {
    fn default() -> Self {
        Self {
            kind: RootFilesystemKind::Overlay,
            mode: None,
            native_plugin: None,
            disable_default_base_layer: false,
            lowers: Vec::new(),
        }
    }
}

/// The root filesystem kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootFilesystemKind {
    #[default]
    Overlay,
    Native,
}

/// Root filesystem mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RootFilesystemMode {
    Ephemeral,
    ReadOnly,
}

/// A lower (immutable) snapshot layer input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RootLowerInput {
    /// The bundled base filesystem snapshot.
    BundledBaseFilesystem,
    /// A snapshot export (`{ kind: "snapshot-export", source }`).
    #[serde(untagged)]
    SnapshotExport(crate::fs::RootSnapshotExport),
}

// ---------------------------------------------------------------------------
// Mounts
// ---------------------------------------------------------------------------

/// A filesystem mount. `Plain.driver` is an in-process trait object and cannot cross the wire.
pub enum MountConfig {
    /// Plain mount over an in-process [`VirtualFileSystem`] driver.
    Plain {
        path: String,
        driver: Arc<dyn VirtualFileSystem>,
        guest_source: Option<String>,
        guest_fstype: Option<String>,
        read_only: bool,
    },
    /// Native plugin mount (`{ id; config? }`).
    Native {
        path: String,
        plugin: MountPlugin,
        guest_source: Option<String>,
        guest_fstype: Option<String>,
        read_only: bool,
    },
    /// Overlay mount (`{ type: "overlay"; store; mode?; lowers }`).
    Overlay {
        path: String,
        filesystem: OverlayMountConfig,
    },
}

/// A native mount plugin descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountPlugin {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}

/// Mount a host `node_modules` directory into the VM at `/root/node_modules`.
///
/// Rust mirror of TS `nodeModulesMount(...)` (`packages/core/src/host-dir-mount.ts`).
/// This is the explicit, mount-based replacement for the removed `moduleAccessCwd`
/// option: the guest module resolver reads the mounted tree through the kernel VFS,
/// so the caller supplies exactly the `node_modules` directory whose packages should
/// be resolvable in the guest. The mount is read-only.
pub fn node_modules_mount(host_node_modules_dir: impl Into<String>) -> MountConfig {
    MountConfig::Native {
        path: "/root/node_modules".to_string(),
        plugin: MountPlugin {
            id: "host_dir".to_string(),
            config: Some(serde_json::json!({
                "hostPath": host_node_modules_dir.into(),
                "readOnly": true,
            })),
        },
        guest_source: Some(String::from("host_dir")),
        guest_fstype: Some(String::from("host_dir")),
        read_only: true,
    }
}

/// Overlay mount filesystem config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayMountConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub store: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<RootFilesystemMode>,
    pub lowers: Vec<RootLowerInput>,
}

// ---------------------------------------------------------------------------
// Sidecar config
// ---------------------------------------------------------------------------

/// How the client obtains its sidecar handle.
pub enum AgentOsSidecarConfig {
    /// Use (or create) a shared pooled sidecar (`pool` default `"default"`).
    Shared { pool: Option<String> },
    /// Use an explicit sidecar handle.
    Explicit {
        handle: Arc<crate::sidecar::AgentOsSidecar>,
    },
}

// ---------------------------------------------------------------------------
// Schedule driver
// ---------------------------------------------------------------------------

/// The callback fired by a [`ScheduleDriver`] when a schedule entry triggers.
///
/// Mirrors the TS `ScheduleEntry.callback: () => void | Promise<void>`. The cron manager passes a
/// closure that runs one job execution; the driver awaits it (and, for the default driver, reschedules
/// the next cron fire afterwards).
pub type ScheduleCallback = Arc<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// A schedule entry handed to a [`ScheduleDriver`]. Mirrors TS `ScheduleEntry`
/// (`cron/schedule-driver.ts`).
#[derive(Clone)]
pub struct ScheduleEntry {
    /// Unique ID for this job.
    pub id: String,
    /// 5/6/7-field cron expression OR an ISO-8601 one-shot timestamp.
    pub schedule: String,
    /// Called when the schedule fires.
    pub callback: ScheduleCallback,
}

/// Driver-owned scheduling abstraction. Mirrors the TS `ScheduleDriver` interface
/// (`cron/schedule-driver.ts`) exactly: the driver parses the schedule, arms the timer, reschedules
/// cron entries after each fire, and tears everything down on [`ScheduleDriver::dispose`]. This is the
/// documented extension point: a custom driver (deterministic virtual-time test driver, fire-immediately
/// driver, etc.) fully controls timing.
pub trait ScheduleDriver: Send + Sync {
    /// Schedule a callback to fire on a cron expression or at a specific time. Returns a cancellation
    /// handle.
    fn schedule(&self, entry: ScheduleEntry) -> ScheduleHandle;

    /// Cancel a previously scheduled entry.
    fn cancel(&self, handle: &ScheduleHandle);

    /// Tear down all scheduled work.
    fn dispose(&self);
}

/// Handle to a scheduled entry. Mirrors TS `ScheduleHandle { id }`. Identifies the entry to cancel via
/// [`ScheduleDriver::cancel`].
#[derive(Clone)]
pub struct ScheduleHandle {
    pub id: String,
}

/// Default schedule driver backed by `tokio` timers and the system clock.
///
/// Mirrors the TS `TimerScheduleDriver`: for cron expressions it computes the next fire time and arms
/// a single timer, rescheduling after each fire; for one-shot timestamps it fires once and removes the
/// entry. Driver-held timer tasks are tracked so [`ScheduleDriver::cancel`] / [`ScheduleDriver::dispose`]
/// can abort them.
#[derive(Default)]
pub struct TimerScheduleDriver {
    timers: Arc<scc::HashMap<String, tokio_util::sync::CancellationToken>>,
}

impl TimerScheduleDriver {
    pub fn new() -> Self {
        Self {
            timers: Arc::new(scc::HashMap::new()),
        }
    }

    /// Arm the next fire for `entry`. For a one-shot or an exhausted cron the entry is dropped. For a
    /// recurring cron the timer reschedules itself after firing the callback. `cancel` is the per-entry
    /// cancellation token shared with the registry slot.
    fn schedule_next(
        timers: Arc<scc::HashMap<String, tokio_util::sync::CancellationToken>>,
        entry: ScheduleEntry,
        cancel: tokio_util::sync::CancellationToken,
    ) {
        let now = chrono::Utc::now();
        let parsed = match crate::cron::parse_schedule(&entry.schedule) {
            Ok(parsed) => parsed,
            Err(_) => {
                let _ = timers.remove(&entry.id);
                return;
            }
        };
        let is_cron = parsed.is_cron();
        let next = match crate::cron::resolve_next_run(&parsed, now) {
            Some(next) => next,
            None => {
                // No upcoming run (one-shot in the past, or exhausted cron).
                let _ = timers.remove(&entry.id);
                return;
            }
        };

        let delay = (next - now).to_std().unwrap_or(std::time::Duration::ZERO);

        tokio::spawn(async move {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return;
                }
                _ = tokio::time::sleep(delay) => {}
            }
            if cancel.is_cancelled() {
                return;
            }
            // The driver is fire-and-forget; errors are the caller's responsibility.
            (entry.callback)().await;

            if is_cron && timers.contains(&entry.id) {
                Self::schedule_next(Arc::clone(&timers), entry, cancel);
            } else {
                let _ = timers.remove(&entry.id);
            }
        });
    }
}

impl ScheduleDriver for TimerScheduleDriver {
    fn schedule(&self, entry: ScheduleEntry) -> ScheduleHandle {
        let id = entry.id.clone();
        let cancel = tokio_util::sync::CancellationToken::new();
        // Replace any existing timer for this id, cancelling it first.
        if let Some((_, old)) = self.timers.remove(&id) {
            old.cancel();
        }
        let _ = self.timers.insert(id.clone(), cancel.clone());

        Self::schedule_next(Arc::clone(&self.timers), entry, cancel);

        ScheduleHandle { id }
    }

    fn cancel(&self, handle: &ScheduleHandle) {
        if let Some((_, cancel)) = self.timers.remove(&handle.id) {
            cancel.cancel();
        }
    }

    fn dispose(&self) {
        self.timers.scan(|_, cancel| cancel.cancel());
        self.timers.clear();
    }
}
