//! The `AgentOs` struct (all fields from ADR-001 §3), the `create` builder, and the `shutdown`
//! (dispose) teardown.
//!
//! `AgentOs` is `Arc`-cloneable; all interior state lives behind concurrent maps / atomics /
//! channels so `&self` methods never need an outer lock. Module files add only `impl AgentOs` blocks
//! and never introduce new struct fields.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use scc::HashMap as SccHashMap;
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use agentos_protocol::generated::v1::{
    AcpCallback, AcpCallbackResponse, AcpEvent, AcpHostRequestCallbackResponse,
};
use agentos_protocol::ACP_EXTENSION_NAMESPACE;
use agentos_sidecar_client::wire;
use agentos_vm_config as vm_config;

use crate::config::{
    AgentOsConfig, AgentOsLimits, Binding, Bindings, MountConfig, PermissionMode, Permissions,
    RootFilesystemConfig, RootFilesystemKind, RootFilesystemMode as ConfigRootFilesystemMode,
    RootLowerInput, SidecarJsBridgeCall, SidecarJsBridgeCallback, TimerScheduleDriver,
};
use crate::cron::CronManager;
use crate::error::ClientError;
use crate::process::SYNTHETIC_PID_BASE;
use crate::session::{
    AgentExitEvent, AgentRestartOutcome, DurableSessionEventEntry, EphemeralSessionEventEntry,
    SessionStreamEntry, SessionUpdate,
};
use crate::sidecar::{AgentOsSidecar, AgentOsSidecarPlacement, AgentOsSidecarVmLease};
use crate::transport::{SidecarProcess, WireSidecarCallback};
use agentos_sidecar_client::TransportError;

use once_cell::sync::OnceCell;

// ---------------------------------------------------------------------------
// Registry entries
// ---------------------------------------------------------------------------

/// An SDK-spawned process (TS `_processes` value). Keyed by user-facing pid.
pub(crate) struct ProcessEntry {
    pub command: String,
    pub args: Vec<String>,
    #[allow(dead_code)]
    pub stdout_tx: broadcast::Sender<Vec<u8>>,
    #[allow(dead_code)]
    pub stderr_tx: broadcast::Sender<Vec<u8>>,
    pub output_tx: broadcast::Sender<crate::process::ProcessOutput>,
    /// Seeded `None`; the already-exited branch fires immediately once it holds `Some(code)`.
    pub exit_tx: watch::Sender<Option<i32>>,
    /// The sidecar-side process id used on the wire.
    pub process_id: String,
    /// The kernel pid returned by the `Execute` response, seeded once the spawn lands. The TS native
    /// path builds `displayPidByKernelPid` from this so `all_processes`/`process_tree` report the
    /// public spawn pid (the map key) for the spawned root, not the raw kernel pid.
    pub kernel_pid: watch::Sender<Option<u32>>,
    /// Handles for the per-process output-callback tasks seeded at spawn (`on_stdout`/`on_stderr`).
    /// The entry retains its own `stdout_tx`/`stderr_tx` clones for late subscribers, so these tasks
    /// never observe the broadcast `Closed`; `shutdown` aborts them when draining the registry.
    pub output_tasks: Vec<JoinHandle<()>>,
    /// Epoch milliseconds captured when `spawn` registered this process (TS `Date.now()`).
    pub started_at: i64,
}

/// A PTY-backed shell (TS `_shells` value). Keyed by synthetic `shell-N` id.
///
/// `data_tx` carries stdout and stderr in their original wire order for terminal renderers.
/// `stderr_tx` is an optional channel-specific diagnostic tap backing the `on_stderr` option and
/// `on_shell_stderr`; terminal consumers must not render both streams or stderr would be duplicated.
pub(crate) struct ShellEntry {
    pub pid: u32,
    pub data_tx: broadcast::Sender<Vec<u8>>,
    pub stderr_tx: broadcast::Sender<Vec<u8>>,
    /// The sidecar-side process id used on the wire.
    pub process_id: String,
    /// Spawn-readiness gate. Seeded `false`; flips to `true` once the background `Execute` request is
    /// acked. TS `openShell` is fully synchronous so `writeShell` always addresses a live spawn; the
    /// Rust wire spawn is async, so `write_shell`/`close_shell` await this gate before issuing their
    /// wire request to preserve the deterministic ordering and avoid dropping early input.
    pub spawned_tx: watch::Sender<bool>,
    /// Exit-code channel backing `wait_shell` (TS `ShellHandle.wait`). Seeded `None`; the background
    /// event loop publishes `Some(exit_code)` when the shell process exits.
    pub exit_tx: watch::Sender<Option<i32>>,
}

/// A connected ACP terminal process and its output fan-out task.
pub(crate) struct AcpTerminalEntry {
    pub exit_task: JoinHandle<()>,
}

/// Mutable output state of a host-request ACP terminal (mirrors the TS `AcpTerminalEntry`
/// `output` / `truncated` accumulation behavior).
pub(crate) struct HostAcpTerminalOutput {
    /// Accumulated UTF-8 terminal output (stdout + stderr interleaved, like the TS handle).
    pub buffer: String,
    pub truncated: bool,
    /// Byte limit; `output` is trimmed from the front once it exceeds this. Mirrors the TS
    /// `outputByteLimit` (default 1 MiB).
    pub output_byte_limit: usize,
}

/// A host-request ACP terminal created via `terminal/create` (mirrors the TS `_acpTerminals`
/// value). Backed by a real PTY shell (`open_shell`); the background fan-out task accumulates
/// output and records the exit code.
pub(crate) struct HostAcpTerminal {
    /// The backing shell id (`shell-N`) used for `terminal/write` / `terminal/resize` /
    /// `terminal/kill`.
    pub shell_id: String,
    /// Shared output buffer updated by the fan-out task and read by `terminal/output`.
    pub output: Arc<parking_lot::Mutex<HostAcpTerminalOutput>>,
    /// Exit code once the process has exited (`None` while running). Mirrors `exitCode`.
    pub exit_rx: watch::Receiver<Option<i32>>,
}

// ---------------------------------------------------------------------------
// AgentOs
// ---------------------------------------------------------------------------

/// A self-contained agentOS package to link into a running VM via
/// [`AgentOs::link_software`]. `path` is normally the packed `.aospkg` file;
/// a directory is accepted only for local transition fixtures. The descriptor
/// is forwarded to the sidecar, which owns the `/opt/agentos` projection and
/// reads package metadata from the packed vbare manifest.
#[derive(Debug, Clone)]
pub struct PackageDescriptor {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedAgent {
    pub id: String,
    pub acp_entrypoint: String,
    pub adapter_entrypoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SoftwareInfo {
    #[serde(rename = "packageName")]
    pub package_name: String,
    pub commands: Vec<String>,
}

/// The high-level client. Cheaply cloneable via `Arc`.
#[derive(Clone)]
pub struct AgentOs {
    inner: Arc<AgentOsInner>,
}

pub(crate) struct AgentOsInner {
    // Transport / connection / VM handle.
    pub(crate) transport: Arc<SidecarProcess>,
    pub(crate) connection_id: String,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    /// Projected command names and guest entrypoints reported by the sidecar.
    pub(crate) projected_commands: parking_lot::Mutex<BTreeMap<String, String>>,
    /// Projected agents reported by the sidecar.
    pub(crate) projected_agents: parking_lot::Mutex<Vec<ProjectedAgent>>,

    // Process registries.
    pub(crate) process_registry_lock: parking_lot::Mutex<()>,
    pub(crate) processes: SccHashMap<u32, ProcessEntry>,
    /// Wire `process_id` allocator for `exec` (the kernel-process view). Distinct from the
    /// spawn synthetic-pid space so an `exec` call never perturbs the observable `spawn` pid sequence
    /// (TS `nextSyntheticPid` is advanced only by `spawn`, never by `exec`).
    pub(crate) process_counter: AtomicU64,
    /// Synthetic display-pid allocator for `spawn` (TS `nextSyntheticPid`, seeded at
    /// [`crate::process::SYNTHETIC_PID_BASE`]). The first spawned process gets `SYNTHETIC_PID_BASE`.
    pub(crate) synthetic_pid_counter: AtomicU64,
    pub(crate) observed_process_time_lock: parking_lot::Mutex<()>,
    /// First-observed start time (epoch ms) per `"<process_id>:<kernel_pid>"`, mirroring TS
    /// `observedProcessStartTimes`. A process keeps the timestamp first seen in `all_processes` across
    /// later calls instead of advancing on every snapshot.
    pub(crate) observed_process_start_times: SccHashMap<String, f64>,
    /// First-observed exit time (epoch ms) per SDK-spawned wire `process_id`, mirroring TS
    /// `tracked.exitTime` (set once when the process is first seen exited).
    pub(crate) observed_process_exit_times: SccHashMap<String, f64>,

    // Shell registries.
    pub(crate) shells: SccHashMap<String, ShellEntry>,
    pub(crate) shell_counter: AtomicU64,
    pub(crate) pending_shell_exits: SccHashMap<u64, JoinHandle<()>>,
    /// Bounded ordered map (cap [`crate::CLOSED_SHELL_EXIT_CODE_RETENTION_LIMIT`]) of exited shells'
    /// exit codes, so `wait_shell` issued after the shell already exited (entry dropped from
    /// `shells`) still resolves with the recorded code — mirrors the TS `_closedShellIds` retention.
    pub(crate) closed_shell_exit_codes: parking_lot::Mutex<VecDeque<(String, i32)>>,
    pub(crate) acp_terminals: SccHashMap<String, AcpTerminalEntry>,
    pub(crate) acp_terminal_count: AtomicUsize,
    pub(crate) acp_terminal_lifecycle_lock: tokio::sync::Mutex<()>,
    /// Host-request ACP terminals created via `terminal/create` (TS `_acpTerminals`). Keyed by the
    /// `acp-terminal-N` id the agent uses in subsequent `terminal/*` calls.
    pub(crate) host_acp_terminals: SccHashMap<String, HostAcpTerminal>,
    /// Monotonic counter for the `acp-terminal-N` ids (TS `_acpTerminalCounter`).
    pub(crate) host_acp_terminal_counter: AtomicU64,

    // Durable session event fan-out. Session state itself is sidecar-owned SQLite.
    pub(crate) durable_session_event_tx: broadcast::Sender<crate::session::SessionStreamEntry>,
    pub(crate) durable_agent_exit_tx: broadcast::Sender<crate::session::AgentExitEvent>,

    // Cron.
    pub(crate) cron: Arc<CronManager>,

    // Config / lifecycle.
    pub(crate) config: Arc<AgentOsConfig>,
    pub(crate) sidecar: Arc<AgentOsSidecar>,
    pub(crate) sidecar_lease: parking_lot::Mutex<Option<AgentOsSidecarVmLease>>,
    pub(crate) dynamic_mounts: parking_lot::Mutex<Vec<wire::MountDescriptor>>,
    pub(crate) disposed: AtomicBool,
    /// Handle for the background ACP event-pump task (`spawn_acp_event_pump`). Stored so `shutdown`
    /// can abort it; the pump only exits on its own when the shared transport's event channel closes,
    /// which does not happen while sibling VMs keep the transport alive. Mirrors `pending_shell_exits`.
    pub(crate) acp_event_pump: parking_lot::Mutex<Option<JoinHandle<()>>>,
}

impl AgentOs {
    /// The sole public VM entry point. Processes software, spawns/authenticates the sidecar, creates
    /// the VM, waits for ready (10s), configures it, takes a lease, and constructs the cron manager
    /// (default [`crate::config::TimerScheduleDriver`]).
    pub async fn create(options: AgentOsConfig) -> Result<AgentOs, ClientError> {
        let config = Arc::new(options);

        // 1. Resolve the sidecar handle (shared "default" pool unless configured otherwise) and
        //    establish/reuse its shared process + authenticated connection. A shared sidecar hosts
        //    multiple VMs in one process, each opening its own session + VM below.
        let sidecar = match &config.sidecar {
            Some(crate::config::AgentOsSidecarConfig::Explicit { handle }) => handle.clone(),
            Some(crate::config::AgentOsSidecarConfig::Shared { pool }) => {
                AgentOs::get_shared_sidecar(pool.clone(), config.sidecar_binary_path.clone())
                    .await?
            }
            None => AgentOs::get_shared_sidecar(None, config.sidecar_binary_path.clone()).await?,
        };
        let (transport, connection_id, _) = sidecar.ensure_connection().await?;

        // 2. Open a session for this VM (connection scope) on the shared connection.
        let session = match transport
            .request_wire(
                wire_connection_ownership(&connection_id),
                wire::RequestPayload::OpenSessionRequest(wire::OpenSessionRequest {
                    placement: sidecar_wire_placement(&sidecar),
                    metadata: HashMap::new(),
                }),
            )
            .await?
        {
            wire::ResponsePayload::SessionOpenedResponse(opened) => opened,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(rejected_to_error(rejected));
            }
            wire::ResponsePayload::AuthenticatedResponse(_)
            | wire::ResponsePayload::VmCreatedResponse(_)
            | wire::ResponsePayload::VmDisposedResponse(_)
            | wire::ResponsePayload::RootFilesystemBootstrappedResponse(_)
            | wire::ResponsePayload::VmConfiguredResponse(_)
            | wire::ResponsePayload::HostCallbacksRegisteredResponse(_)
            | wire::ResponsePayload::LayerCreatedResponse(_)
            | wire::ResponsePayload::LayerSealedResponse(_)
            | wire::ResponsePayload::SnapshotImportedResponse(_)
            | wire::ResponsePayload::SnapshotExportedResponse(_)
            | wire::ResponsePayload::OverlayCreatedResponse(_)
            | wire::ResponsePayload::GuestFilesystemResultResponse(_)
            | wire::ResponsePayload::RootFilesystemSnapshotResponse(_)
            | wire::ResponsePayload::ProcessStartedResponse(_)
            | wire::ResponsePayload::StdinWrittenResponse(_)
            | wire::ResponsePayload::PtyResizedResponse(_)
            | wire::ResponsePayload::StdinClosedResponse(_)
            | wire::ResponsePayload::ProcessKilledResponse(_)
            | wire::ResponsePayload::ProcessSnapshotResponse(_)
            | wire::ResponsePayload::ListenerSnapshotResponse(_)
            | wire::ResponsePayload::BoundUdpSnapshotResponse(_)
            | wire::ResponsePayload::SignalStateResponse(_)
            | wire::ResponsePayload::ZombieTimerCountResponse(_)
            | wire::ResponsePayload::FilesystemResultResponse(_)
            | wire::ResponsePayload::PermissionDecisionResponse(_)
            | wire::ResponsePayload::PersistenceStateResponse(_)
            | wire::ResponsePayload::PersistenceFlushedResponse(_)
            | wire::ResponsePayload::VmFetchResponse(_)
            | wire::ResponsePayload::ExtEnvelope(_)
            | wire::ResponsePayload::GuestKernelResultResponse(_)
            | wire::ResponsePayload::ResourceSnapshotResponse(_)
            | wire::ResponsePayload::PackageLinkedResponse(_)
            | wire::ResponsePayload::ProvidedCommandsResponse(_)
            | wire::ResponsePayload::ListMountsResponse(_)
            | wire::ResponsePayload::ExecutionAcceptedResponse(_)
            | wire::ResponsePayload::ExecutionCompletedResponse(_)
            | wire::ResponsePayload::ExecutionEvaluationResponse(_)
            | wire::ResponsePayload::TypeScriptCheckResponse(_)
            | wire::ResponsePayload::ExecutionDescriptorResponse(_)
            | wire::ResponsePayload::ExecutionListResponse(_)
            | wire::ResponsePayload::ExecutionDeletedResponse(_)
            | wire::ResponsePayload::ExecutionIoResponse(_)
            | wire::ResponsePayload::ExecutionOutputPageResponse(_) => {
                return Err(ClientError::Sidecar(
                    "unexpected open_session response".to_string(),
                ));
            }
        };
        let session_id = session.session_id;

        // 3. Subscribe to events BEFORE CreateVm so the `ready` lifecycle event cannot be missed.
        let mut events = transport.subscribe_wire_events();
        let permissions = permissions_policy(&config);
        let create_vm_config = serialize_create_vm_config_for_sidecar(&config)?;
        if let Some(callback) = config.sidecar_js_bridge_callback.clone() {
            let _ = session_js_bridge_callbacks()
                .insert(sidecar_session_key(&connection_id, &session_id), callback);
            transport.register_wire_callback("js_bridge_call", js_bridge_call_callback());
        }

        // 4. Create the VM (session scope).
        let vm = match transport
            .request_wire(
                wire_session_ownership(&connection_id, &session_id),
                wire::RequestPayload::CreateVmRequest(wire::CreateVmRequest {
                    runtime: wire::GuestRuntimeKind::JavaScript,
                    config: serde_json::to_string(&create_vm_config).map_err(|error| {
                        ClientError::Sidecar(format!(
                            "failed to serialize create VM config: {error}"
                        ))
                    })?,
                }),
            )
            .await?
        {
            wire::ResponsePayload::VmCreatedResponse(created) => created,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(rejected_to_error(rejected));
            }
            wire::ResponsePayload::AuthenticatedResponse(_)
            | wire::ResponsePayload::SessionOpenedResponse(_)
            | wire::ResponsePayload::VmDisposedResponse(_)
            | wire::ResponsePayload::RootFilesystemBootstrappedResponse(_)
            | wire::ResponsePayload::VmConfiguredResponse(_)
            | wire::ResponsePayload::HostCallbacksRegisteredResponse(_)
            | wire::ResponsePayload::LayerCreatedResponse(_)
            | wire::ResponsePayload::LayerSealedResponse(_)
            | wire::ResponsePayload::SnapshotImportedResponse(_)
            | wire::ResponsePayload::SnapshotExportedResponse(_)
            | wire::ResponsePayload::OverlayCreatedResponse(_)
            | wire::ResponsePayload::GuestFilesystemResultResponse(_)
            | wire::ResponsePayload::RootFilesystemSnapshotResponse(_)
            | wire::ResponsePayload::ProcessStartedResponse(_)
            | wire::ResponsePayload::StdinWrittenResponse(_)
            | wire::ResponsePayload::PtyResizedResponse(_)
            | wire::ResponsePayload::StdinClosedResponse(_)
            | wire::ResponsePayload::ProcessKilledResponse(_)
            | wire::ResponsePayload::ProcessSnapshotResponse(_)
            | wire::ResponsePayload::ListenerSnapshotResponse(_)
            | wire::ResponsePayload::BoundUdpSnapshotResponse(_)
            | wire::ResponsePayload::SignalStateResponse(_)
            | wire::ResponsePayload::ZombieTimerCountResponse(_)
            | wire::ResponsePayload::FilesystemResultResponse(_)
            | wire::ResponsePayload::PermissionDecisionResponse(_)
            | wire::ResponsePayload::PersistenceStateResponse(_)
            | wire::ResponsePayload::PersistenceFlushedResponse(_)
            | wire::ResponsePayload::VmFetchResponse(_)
            | wire::ResponsePayload::ExtEnvelope(_)
            | wire::ResponsePayload::GuestKernelResultResponse(_)
            | wire::ResponsePayload::ResourceSnapshotResponse(_)
            | wire::ResponsePayload::PackageLinkedResponse(_)
            | wire::ResponsePayload::ProvidedCommandsResponse(_)
            | wire::ResponsePayload::ListMountsResponse(_)
            | wire::ResponsePayload::ExecutionAcceptedResponse(_)
            | wire::ResponsePayload::ExecutionCompletedResponse(_)
            | wire::ResponsePayload::ExecutionEvaluationResponse(_)
            | wire::ResponsePayload::TypeScriptCheckResponse(_)
            | wire::ResponsePayload::ExecutionDescriptorResponse(_)
            | wire::ResponsePayload::ExecutionListResponse(_)
            | wire::ResponsePayload::ExecutionDeletedResponse(_)
            | wire::ResponsePayload::ExecutionIoResponse(_)
            | wire::ResponsePayload::ExecutionOutputPageResponse(_) => {
                return Err(ClientError::Sidecar(
                    "unexpected create_vm response".to_string(),
                ));
            }
        };
        let vm_id = vm.vm_id;

        // 5. Wait for the VM to reach `ready` (bounded by VM_READY_TIMEOUT_MS).
        wait_for_vm_ready(&mut events, &vm_id, crate::VM_READY_TIMEOUT_MS).await?;

        // Forward package dirs to the sidecar. The sidecar owns manifest parsing,
        // command discovery, and agent enumeration for the `/opt/agentos` projection.
        let packages = build_package_descriptors(&config);

        // Native plugin mounts configured on the client.
        let mounts = serialize_mounts(&config)?;
        let configured_mounts = mounts.clone();

        // 6. Configure the VM (vm scope). The sidecar owns the `/opt/agentos` package
        // projection: it builds the staging dir + registers the read-only host_dir
        // mount itself from the forwarded `packages`.
        let (projected_commands, projected_agents) = match transport
            .request_wire(
                wire_vm_ownership(&connection_id, &session_id, &vm_id),
                wire::RequestPayload::ConfigureVmRequest(wire::ConfigureVmRequest {
                    mounts,
                    // The legacy `software`/SoftwareDescriptor provisioning path is
                    // retired: all boot software is projected via `packages`.
                    software: Vec::new(),
                    permissions: Some(permissions),
                    // Client-side `moduleAccessCwd` was removed in favor of an
                    // explicit `nodeModulesMount(...)` entry in `mounts`; the
                    // agentos wire field is left unset.
                    module_access_cwd: None,
                    instructions: config.additional_instructions.clone().into_iter().collect(),
                    projected_modules: Vec::new(),
                    command_permissions: HashMap::new(),
                    loopback_exempt_ports: config.loopback_exempt_ports.clone(),
                    packages,
                    packages_mount_at: config.packages_mount_at.clone().unwrap_or_default(),
                    bootstrap_commands: Vec::new(),
                    binding_shim_commands: Vec::new(),
                }),
            )
            .await?
        {
            wire::ResponsePayload::VmConfiguredResponse(configured) => (
                configured
                    .projected_commands
                    .into_iter()
                    .map(|command| (command.name, command.guest_path))
                    .collect(),
                projected_agents_from_wire(configured.agents),
            ),
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(rejected_to_error(rejected));
            }
            wire::ResponsePayload::AuthenticatedResponse(_)
            | wire::ResponsePayload::SessionOpenedResponse(_)
            | wire::ResponsePayload::VmCreatedResponse(_)
            | wire::ResponsePayload::VmDisposedResponse(_)
            | wire::ResponsePayload::RootFilesystemBootstrappedResponse(_)
            | wire::ResponsePayload::HostCallbacksRegisteredResponse(_)
            | wire::ResponsePayload::LayerCreatedResponse(_)
            | wire::ResponsePayload::LayerSealedResponse(_)
            | wire::ResponsePayload::SnapshotImportedResponse(_)
            | wire::ResponsePayload::SnapshotExportedResponse(_)
            | wire::ResponsePayload::OverlayCreatedResponse(_)
            | wire::ResponsePayload::GuestFilesystemResultResponse(_)
            | wire::ResponsePayload::RootFilesystemSnapshotResponse(_)
            | wire::ResponsePayload::ProcessStartedResponse(_)
            | wire::ResponsePayload::StdinWrittenResponse(_)
            | wire::ResponsePayload::PtyResizedResponse(_)
            | wire::ResponsePayload::StdinClosedResponse(_)
            | wire::ResponsePayload::ProcessKilledResponse(_)
            | wire::ResponsePayload::ProcessSnapshotResponse(_)
            | wire::ResponsePayload::ListenerSnapshotResponse(_)
            | wire::ResponsePayload::BoundUdpSnapshotResponse(_)
            | wire::ResponsePayload::SignalStateResponse(_)
            | wire::ResponsePayload::ZombieTimerCountResponse(_)
            | wire::ResponsePayload::FilesystemResultResponse(_)
            | wire::ResponsePayload::PermissionDecisionResponse(_)
            | wire::ResponsePayload::PersistenceStateResponse(_)
            | wire::ResponsePayload::PersistenceFlushedResponse(_)
            | wire::ResponsePayload::VmFetchResponse(_)
            | wire::ResponsePayload::ExtEnvelope(_)
            | wire::ResponsePayload::GuestKernelResultResponse(_)
            | wire::ResponsePayload::ResourceSnapshotResponse(_)
            | wire::ResponsePayload::PackageLinkedResponse(_)
            | wire::ResponsePayload::ProvidedCommandsResponse(_)
            | wire::ResponsePayload::ListMountsResponse(_)
            | wire::ResponsePayload::ExecutionAcceptedResponse(_)
            | wire::ResponsePayload::ExecutionCompletedResponse(_)
            | wire::ResponsePayload::ExecutionEvaluationResponse(_)
            | wire::ResponsePayload::TypeScriptCheckResponse(_)
            | wire::ResponsePayload::ExecutionDescriptorResponse(_)
            | wire::ResponsePayload::ExecutionListResponse(_)
            | wire::ResponsePayload::ExecutionDeletedResponse(_)
            | wire::ResponsePayload::ExecutionIoResponse(_)
            | wire::ResponsePayload::ExecutionOutputPageResponse(_) => {
                return Err(ClientError::Sidecar(
                    "unexpected configure_vm response".to_string(),
                ));
            }
        };

        // 6b. Register host binding kits (if any): forward each binding definition via `register_host_callbacks`,
        //     record the host execute callbacks in the per-VM registry, and install the shared
        //     host-callback that routes guest binding calls back to the host by VM.
        if !config.bindings.is_empty() {
            let mut binding_map: HashMap<String, Binding> = HashMap::new();
            for collection in &config.bindings {
                let mut bindings = HashMap::new();
                for binding in &collection.bindings {
                    bindings.insert(
                        binding.name.clone(),
                        wire::RegisteredHostCallbackDefinition {
                            description: binding.description.clone(),
                            input_schema: json_utf8(
                                &binding.input_schema,
                                "host callback input schema",
                            )?,
                            timeout_ms: binding.timeout_ms,
                            examples: Vec::new(),
                        },
                    );
                    binding_map.insert(
                        format!("{}:{}", collection.name, binding.name),
                        binding.clone(),
                    );
                }
                match transport
                    .request_wire(
                        wire_vm_ownership(&connection_id, &session_id, &vm_id),
                        wire::RequestPayload::RegisterHostCallbacksRequest(
                            wire::RegisterHostCallbacksRequest {
                                name: collection.name.clone(),
                                description: collection.description.clone(),
                                command_aliases: vec![format!("agentos-{}", collection.name)],
                                registry_command_aliases: vec![String::from("agentos")],
                                callbacks: bindings,
                            },
                        ),
                    )
                    .await?
                {
                    wire::ResponsePayload::HostCallbacksRegisteredResponse(_) => {}
                    wire::ResponsePayload::RejectedResponse(rejected) => {
                        return Err(rejected_to_error(rejected));
                    }
                    wire::ResponsePayload::AuthenticatedResponse(_)
                    | wire::ResponsePayload::SessionOpenedResponse(_)
                    | wire::ResponsePayload::VmCreatedResponse(_)
                    | wire::ResponsePayload::VmDisposedResponse(_)
                    | wire::ResponsePayload::RootFilesystemBootstrappedResponse(_)
                    | wire::ResponsePayload::VmConfiguredResponse(_)
                    | wire::ResponsePayload::LayerCreatedResponse(_)
                    | wire::ResponsePayload::LayerSealedResponse(_)
                    | wire::ResponsePayload::SnapshotImportedResponse(_)
                    | wire::ResponsePayload::SnapshotExportedResponse(_)
                    | wire::ResponsePayload::OverlayCreatedResponse(_)
                    | wire::ResponsePayload::GuestFilesystemResultResponse(_)
                    | wire::ResponsePayload::RootFilesystemSnapshotResponse(_)
                    | wire::ResponsePayload::ProcessStartedResponse(_)
                    | wire::ResponsePayload::StdinWrittenResponse(_)
                    | wire::ResponsePayload::PtyResizedResponse(_)
                    | wire::ResponsePayload::StdinClosedResponse(_)
                    | wire::ResponsePayload::ProcessKilledResponse(_)
                    | wire::ResponsePayload::ProcessSnapshotResponse(_)
                    | wire::ResponsePayload::ListenerSnapshotResponse(_)
                    | wire::ResponsePayload::BoundUdpSnapshotResponse(_)
                    | wire::ResponsePayload::SignalStateResponse(_)
                    | wire::ResponsePayload::ZombieTimerCountResponse(_)
                    | wire::ResponsePayload::FilesystemResultResponse(_)
                    | wire::ResponsePayload::PermissionDecisionResponse(_)
                    | wire::ResponsePayload::PersistenceStateResponse(_)
                    | wire::ResponsePayload::PersistenceFlushedResponse(_)
                    | wire::ResponsePayload::VmFetchResponse(_)
                    | wire::ResponsePayload::ExtEnvelope(_)
                    | wire::ResponsePayload::GuestKernelResultResponse(_)
                    | wire::ResponsePayload::ResourceSnapshotResponse(_)
                    | wire::ResponsePayload::PackageLinkedResponse(_)
                    | wire::ResponsePayload::ProvidedCommandsResponse(_)
                    | wire::ResponsePayload::ListMountsResponse(_)
                    | wire::ResponsePayload::ExecutionAcceptedResponse(_)
                    | wire::ResponsePayload::ExecutionCompletedResponse(_)
                    | wire::ResponsePayload::ExecutionEvaluationResponse(_)
                    | wire::ResponsePayload::TypeScriptCheckResponse(_)
                    | wire::ResponsePayload::ExecutionDescriptorResponse(_)
                    | wire::ResponsePayload::ExecutionListResponse(_)
                    | wire::ResponsePayload::ExecutionDeletedResponse(_)
                    | wire::ResponsePayload::ExecutionIoResponse(_)
                    | wire::ResponsePayload::ExecutionOutputPageResponse(_) => {
                        return Err(ClientError::Sidecar(
                            "unexpected register_host_callbacks response".to_string(),
                        ));
                    }
                }
            }
            let _ = vm_bindings().insert(
                vm_id.clone(),
                Arc::new(VmBindingRegistry {
                    bindings: config.bindings.clone(),
                    binding_map,
                    permissions: config.permissions.clone(),
                }),
            );
            transport.register_wire_callback("host_callback", host_callback_callback());
        }

        // 7. Lease this VM on the (possibly shared) sidecar, build cron, and assemble the client.
        sidecar.active_vm_count.fetch_add(1, Ordering::SeqCst);
        let lease = AgentOsSidecarVmLease {
            sidecar: sidecar.clone(),
        };

        let driver = config
            .schedule_driver
            .clone()
            .unwrap_or_else(|| Arc::new(TimerScheduleDriver::new()));
        let cron = Arc::new(CronManager::new(driver));

        let inner = AgentOsInner {
            transport,
            connection_id,
            session_id,
            vm_id,
            projected_commands: parking_lot::Mutex::new(projected_commands),
            projected_agents: parking_lot::Mutex::new(projected_agents),
            process_registry_lock: parking_lot::Mutex::new(()),
            processes: SccHashMap::new(),
            process_counter: AtomicU64::new(1),
            synthetic_pid_counter: AtomicU64::new(SYNTHETIC_PID_BASE),
            observed_process_time_lock: parking_lot::Mutex::new(()),
            observed_process_start_times: SccHashMap::new(),
            observed_process_exit_times: SccHashMap::new(),
            shells: SccHashMap::new(),
            shell_counter: AtomicU64::new(0),
            pending_shell_exits: SccHashMap::new(),
            closed_shell_exit_codes: parking_lot::Mutex::new(VecDeque::new()),
            acp_terminals: SccHashMap::new(),
            acp_terminal_count: AtomicUsize::new(0),
            acp_terminal_lifecycle_lock: tokio::sync::Mutex::new(()),
            host_acp_terminals: SccHashMap::new(),
            host_acp_terminal_counter: AtomicU64::new(0),
            durable_session_event_tx: broadcast::channel(1024).0,
            durable_agent_exit_tx: broadcast::channel(64).0,
            cron,
            config,
            sidecar,
            sidecar_lease: parking_lot::Mutex::new(Some(lease)),
            dynamic_mounts: parking_lot::Mutex::new(configured_mounts),
            disposed: AtomicBool::new(false),
            acp_event_pump: parking_lot::Mutex::new(None),
        };

        let client = AgentOs {
            inner: Arc::new(inner),
        };
        // Register the ACP host-operation router unconditionally. Adapters can
        // request filesystem or terminal work even when no binding kit exists.
        // Re-registering on a shared transport replaces the same stateless callback.
        let _ = vm_acp_routers().insert(client.inner.vm_id.clone(), Arc::downgrade(&client.inner));
        client
            .inner
            .transport
            .register_wire_callback("ext", acp_host_callback());
        spawn_acp_event_pump(&client);
        Ok(client)
    }

    /// Dispose the VM (= TS `dispose`). Teardown order:
    /// 1. cron dispose
    /// 2. kill all shells + snapshot pending exits
    /// 3. kill all ACP terminals
    /// 4. drain tracked shell-exit tasks (two-phase, bounded by
    ///    [`crate::SHELL_DISPOSE_TIMEOUT_MS`])
    /// 5. unregister the sidecar event listener
    /// 6. release the lease (or tear down the transport)
    ///
    /// Idempotent (guarded by `disposed`).
    /// Dynamically link a software package into the RUNNING VM (parity with the
    /// TS client's `linkSoftware`). Forwarded to the sidecar, which owns the
    /// `/opt/agentos` projection and appends the package to its live staging dir,
    /// so the package's commands appear under `/opt/agentos/bin` (on `$PATH`)
    /// immediately with no reboot. Errors if a command name is already linked.
    pub async fn link_software(&self, descriptor: PackageDescriptor) -> Result<(), ClientError> {
        let inner = self.inner();
        let response = self
            .transport()
            .request_wire(
                wire_vm_ownership(&inner.connection_id, &inner.session_id, &inner.vm_id),
                wire::RequestPayload::LinkPackageRequest(wire::LinkPackageRequest {
                    // The wire `PackageDescriptor` carries the packed package
                    // `path`; the sidecar reads metadata from that payload.
                    package: wire::PackageDescriptor {
                        path: descriptor.path,
                    },
                }),
            )
            .await?;
        match response {
            wire::ResponsePayload::PackageLinkedResponse(linked) => {
                let mut guard = inner.projected_commands.lock();
                for command in linked.projected_commands {
                    guard.insert(command.name, command.guest_path);
                }
                register_projected_agents(
                    &inner.projected_agents,
                    projected_agents_from_wire(linked.agents),
                );
                Ok(())
            }
            wire::ResponsePayload::RejectedResponse(rejected) => Err(rejected_to_error(rejected)),
            other => Err(ClientError::Sidecar(format!(
                "unexpected link_package response: {other:?}"
            ))),
        }
    }

    pub async fn list_software(&self) -> Result<Vec<SoftwareInfo>, ClientError> {
        let inner = self.inner();
        let response = self
            .transport()
            .request_wire(
                wire_vm_ownership(&inner.connection_id, &inner.session_id, &inner.vm_id),
                wire::RequestPayload::ProvidedCommandsRequest,
            )
            .await?;
        match response {
            wire::ResponsePayload::ProvidedCommandsResponse(provided) => Ok(provided
                .packages
                .into_iter()
                .map(|package| SoftwareInfo {
                    package_name: package.package_name,
                    commands: package.commands,
                })
                .collect()),
            wire::ResponsePayload::RejectedResponse(rejected) => Err(rejected_to_error(rejected)),
            other => Err(ClientError::Sidecar(format!(
                "unexpected list_software response: {other:?}"
            ))),
        }
    }

    pub async fn shutdown(&self) -> Result<(), ClientError> {
        // Idempotent: only the first caller runs teardown.
        if self.inner.disposed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        // The `/opt/agentos` projection staging dir is owned + cleaned up by the
        // sidecar on VM dispose, so the client no longer removes it here.

        // 1. Cron dispose (cancel armed timers + tear down the driver).
        self.inner.cron.dispose();

        // Abort the background ACP event pump and drain the SDK-spawned process registry. Neither
        // ends on its own while a shared transport stays alive: the pump only exits on transport
        // close, and the per-process output tasks await a broadcast `Closed` that the entry's own
        // retained sender clones prevent. Aborting + clearing here stops both from leaking past
        // dispose.
        abort_tracked_task(&self.inner.acp_event_pump);
        crate::process::drain_process_output_tasks(&self.inner.processes);

        // 2-5. Best-effort drain tracked shell and terminal tasks before the VM is disposed, bounded
        //      by SHELL_DISPOSE_TIMEOUT_MS so late output cannot race a closed transport.
        let mut exit_tasks = Vec::new();
        self.inner.pending_shell_exits.retain(|_, task| {
            exit_tasks.push(std::mem::replace(task, tokio::spawn(async {})));
            false
        });

        {
            let _terminal_lifecycle_guard = self.inner.acp_terminal_lifecycle_lock.lock().await;
            let mut terminal_entries = Vec::new();
            self.inner.acp_terminals.retain(|process_id, entry| {
                terminal_entries.push((
                    process_id.clone(),
                    std::mem::replace(&mut entry.exit_task, tokio::spawn(async {})),
                ));
                false
            });
            self.inner.acp_terminal_count.store(0, Ordering::SeqCst);
            for (process_id, _) in &terminal_entries {
                let transport = self.transport().clone();
                let ownership = wire::OwnershipScope::VmOwnership(wire::VmOwnership {
                    connection_id: self.inner.connection_id.clone(),
                    session_id: self.inner.session_id.clone(),
                    vm_id: self.inner.vm_id.clone(),
                });
                let process_id = process_id.clone();
                exit_tasks.push(tokio::spawn(async move {
                    let _ = transport
                        .request_wire(
                            ownership,
                            wire::RequestPayload::KillProcessRequest(wire::KillProcessRequest {
                                process_id,
                                signal: String::from("SIGTERM"),
                            }),
                        )
                        .await;
                }));
            }
            for (_, task) in terminal_entries {
                exit_tasks.push(task);
            }
        }

        // Tear down host-request ACP terminals (`terminal/create`). Close the backing shell, which
        // sends SIGTERM, removes the shell entry, and ends the fan-out/exit task; the task itself is
        // tracked in `pending_shell_exits` above and drained with the other shell exit tasks.
        let mut host_terminal_shells = Vec::new();
        self.inner.host_acp_terminals.retain(|_, terminal| {
            host_terminal_shells.push(terminal.shell_id.clone());
            false
        });
        for shell_id in host_terminal_shells {
            let _ = self.close_shell(&shell_id);
        }

        if !exit_tasks.is_empty() {
            let mut drain_tasks = exit_tasks;
            if tokio::time::timeout(
                Duration::from_millis(crate::SHELL_DISPOSE_TIMEOUT_MS),
                futures::future::join_all(drain_tasks.iter_mut()),
            )
            .await
            .is_err()
            {
                for task in drain_tasks {
                    task.abort();
                }
            }
        }

        // 6-7. Release this VM (DisposeVm best-effort) and its lease. The transport is shared across
        //      VMs on the same sidecar, so it is only torn down when this was the last VM (matching
        //      the TS lease/shared-sidecar lifecycle); otherwise sibling VMs keep using it.
        let lease = self.inner.sidecar_lease.lock().take();
        let _ = self
            .transport()
            .request_wire(
                wire::OwnershipScope::VmOwnership(wire::VmOwnership {
                    connection_id: self.inner.connection_id.clone(),
                    session_id: self.inner.session_id.clone(),
                    vm_id: self.inner.vm_id.clone(),
                }),
                wire::RequestPayload::DisposeVmRequest(wire::DisposeVmRequest {
                    reason: wire::DisposeReason::Requested,
                }),
            )
            .await;
        let _ = vm_bindings().remove(&self.inner.vm_id);
        let _ = vm_acp_routers().remove(&self.inner.vm_id);
        let _ = session_js_bridge_callbacks().remove(&sidecar_session_key(
            &self.inner.connection_id,
            &self.inner.session_id,
        ));
        let sidecar = self.inner.sidecar.clone();
        if let Some(lease) = lease {
            lease.dispose().await?;
        }
        if sidecar.active_vm_count.load(Ordering::SeqCst) == 0 {
            sidecar.kill_connection().await;
            let _ = sidecar.dispose().await;
        }

        Ok(())
    }

    // --- internal accessors used by sibling impl blocks ---

    pub(crate) fn inner(&self) -> &AgentOsInner {
        &self.inner
    }

    pub(crate) fn transport(&self) -> &Arc<SidecarProcess> {
        &self.inner.transport
    }

    pub(crate) fn connection_id(&self) -> &str {
        &self.inner.connection_id
    }

    pub(crate) fn wire_session_id(&self) -> &str {
        &self.inner.session_id
    }

    pub(crate) fn vm_id(&self) -> &str {
        &self.inner.vm_id
    }

    pub(crate) fn config(&self) -> &Arc<AgentOsConfig> {
        &self.inner.config
    }

    pub(crate) fn cron(&self) -> &Arc<CronManager> {
        &self.inner.cron
    }

    /// The (possibly shared) sidecar handle backing this VM. Public for parity with TS
    /// `AgentOs.sidecar` (e.g. `describe()` reports `active_vm_count` across VMs sharing a pool).
    pub fn sidecar(&self) -> Arc<AgentOsSidecar> {
        self.inner.sidecar.clone()
    }

    pub fn projected_agents(&self) -> Vec<ProjectedAgent> {
        self.inner.projected_agents.lock().clone()
    }
}

/// Abort and clear a single tracked background-task handle (e.g. the ACP event pump) so it cannot
/// outlive the disposed VM. Mirrors the `pending_shell_exits` drain in `shutdown`.
fn abort_tracked_task(slot: &parking_lot::Mutex<Option<JoinHandle<()>>>) {
    if let Some(handle) = slot.lock().take() {
        handle.abort();
    }
}

fn spawn_acp_event_pump(client: &AgentOs) {
    let mut events = client.transport().subscribe_wire_events();
    let inner = Arc::downgrade(&client.inner);
    let handle = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok((ownership, wire::EventPayload::ExtEnvelope(envelope))) => {
                    let Some(inner) = inner.upgrade() else {
                        break;
                    };
                    if inner.disposed.load(Ordering::SeqCst) {
                        break;
                    }
                    if wire_ownership_vm_id(&ownership) != Some(inner.vm_id.as_str()) {
                        continue;
                    }
                    if let Err(error) = deliver_acp_ext_event(&inner, envelope) {
                        tracing::warn!(?error, "failed to deliver acp extension event");
                    }
                }
                Ok((
                    _,
                    wire::EventPayload::VmLifecycleEvent(_)
                    | wire::EventPayload::ProcessOutputEvent(_)
                    | wire::EventPayload::ProcessExitedEvent(_)
                    | wire::EventPayload::ExecutionOutputEvent(_)
                    | wire::EventPayload::ExecutionCompletedEvent(_)
                    | wire::EventPayload::StructuredEvent(_),
                )) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    *client.inner.acp_event_pump.lock() = Some(handle);
}

fn deliver_acp_ext_event(
    inner: &AgentOsInner,
    envelope: wire::ExtEnvelope,
) -> Result<(), ClientError> {
    if envelope.namespace != ACP_EXTENSION_NAMESPACE {
        return Ok(());
    }
    let event: AcpEvent = serde_bare::from_slice(&envelope.payload)
        .map_err(|error| ClientError::Sidecar(format!("invalid ACP event: {error}")))?;
    match event {
        AcpEvent::AcpDurableSessionEvent(event) => {
            let durable_event = crate::session::decode_durable_event(event.event)
                .map_err(|error| ClientError::Sidecar(error.to_string()))?;
            let _ = inner
                .durable_session_event_tx
                .send(SessionStreamEntry::Durable(DurableSessionEventEntry {
                    durability: crate::session::DurableEventKind::Durable,
                    session_id: event.session_id.clone(),
                    sequence: event.sequence,
                    timestamp: event.timestamp,
                    event: durable_event.clone(),
                }));
            Ok(())
        }
        AcpEvent::AcpEphemeralSessionUpdateEvent(event) => {
            let update: SessionUpdate = serde_json::from_str(&event.update).map_err(|error| {
                ClientError::Sidecar(format!("invalid ephemeral ACP session update: {error}"))
            })?;
            let session_event = match update {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    crate::session::EphemeralSessionEvent::AgentMessageChunk(chunk)
                }
                SessionUpdate::AgentThoughtChunk(chunk) => {
                    crate::session::EphemeralSessionEvent::AgentThoughtChunk(chunk)
                }
                _ => {
                    return Err(ClientError::Sidecar(String::from(
                        "ephemeral ACP event must be an agent message or thought chunk",
                    )))
                }
            };
            let _ = inner
                .durable_session_event_tx
                .send(SessionStreamEntry::Ephemeral(EphemeralSessionEventEntry {
                    durability: crate::session::EphemeralEventKind::Ephemeral,
                    session_id: event.session_id,
                    after_sequence: event.after_sequence,
                    event: session_event,
                }));
            Ok(())
        }
        AcpEvent::AcpSessionEvent(event) => {
            tracing::warn!(
                session_id = event.session_id,
                "ignored legacy live-session event; durable session events use typed envelopes"
            );
            Ok(())
        }
        AcpEvent::AcpAgentStderrEvent(event) => {
            let mut stderr = std::io::stderr().lock();
            if let Err(error) = stderr.write_all(&event.chunk).and_then(|_| stderr.flush()) {
                tracing::warn!(?error, "failed to write acp stderr event");
            }
            Ok(())
        }
        AcpEvent::AcpAgentExitedEvent(event) => {
            tracing::warn!(
                session_id = event.session_id,
                agent_type = event.agent_type,
                process_id = event.process_id,
                exit_code = ?event.exit_code,
                restart = event.restart,
                restart_count = event.restart_count,
                max_restarts = event.max_restarts,
                "acp agent adapter exited unexpectedly"
            );
            let _ = inner.durable_agent_exit_tx.send(AgentExitEvent {
                session_id: event.session_id,
                agent_type: event.agent_type,
                process_id: event.process_id,
                pid: event.pid,
                exit_code: event.exit_code,
                restart: AgentRestartOutcome::NotAttempted,
                restart_count: event.restart_count,
                max_restarts: event.max_restarts,
            });
            Ok(())
        }
    }
}

/// Convert a sidecar's client-side placement into the wire `SidecarPlacement` for OpenSession.
fn sidecar_wire_placement(sidecar: &AgentOsSidecar) -> wire::SidecarPlacement {
    match &sidecar.placement {
        AgentOsSidecarPlacement::Shared { pool } => {
            wire::SidecarPlacement::SidecarPlacementShared(wire::SidecarPlacementShared {
                pool: pool.clone(),
            })
        }
        AgentOsSidecarPlacement::Explicit { sidecar_id } => {
            wire::SidecarPlacement::SidecarPlacementExplicit(wire::SidecarPlacementExplicit {
                sidecar_id: sidecar_id.clone(),
            })
        }
    }
}

fn wire_connection_ownership(connection_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::ConnectionOwnership(wire::ConnectionOwnership {
        connection_id: connection_id.to_string(),
    })
}

fn wire_session_ownership(connection_id: &str, session_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::SessionOwnership(wire::SessionOwnership {
        connection_id: connection_id.to_string(),
        session_id: session_id.to_string(),
    })
}

fn wire_vm_ownership(connection_id: &str, session_id: &str, vm_id: &str) -> wire::OwnershipScope {
    wire::OwnershipScope::VmOwnership(wire::VmOwnership {
        connection_id: connection_id.to_string(),
        session_id: session_id.to_string(),
        vm_id: vm_id.to_string(),
    })
}

fn serialize_create_vm_config_for_sidecar(
    config: &AgentOsConfig,
) -> Result<vm_config::CreateVmConfig, ClientError> {
    let (root_filesystem, native_root) =
        serialize_root_filesystem_config_for_sidecar(&config.root_filesystem)?;
    Ok(vm_config::CreateVmConfig {
        database: config.database.clone(),
        cwd: None,
        env: BTreeMap::new(),
        user: config.user.clone(),
        root_filesystem,
        permissions: Some(permissions_policy_config(config)),
        limits: serialize_limits_config_for_sidecar(config.limits.as_ref())?,
        dns: None,
        native_root,
        listen: None,
        loopback_exempt_ports: config.loopback_exempt_ports.clone(),
        // 0.3: the Node builtin allow-list moved from ConfigureVmRequest to
        // VM creation. `None` => engine default allow-list; `Some([..])` =>
        // exactly those (`Some([])` denies all). Platform/module-resolution
        // keep their engine defaults (full Node emulation), matching prior
        // behavior where Agent OS only ever constrained the builtin allow-list.
        js_runtime: config.allowed_node_builtins.as_ref().map(|allowed| {
            vm_config::JsRuntimeConfig {
                platform: vm_config::JsRuntimePlatform::default(),
                module_resolution: vm_config::JsModuleResolution::default(),
                allowed_builtins: Some(allowed.clone()),
                high_resolution_time: None,
            }
        }),
        bootstrap_commands: Some(vec![
            String::from("node"),
            String::from("npm"),
            String::from("npx"),
            String::from("python"),
            String::from("python3"),
        ]),
    })
}

fn serialize_root_filesystem_config_for_sidecar(
    config: &RootFilesystemConfig,
) -> Result<
    (
        vm_config::RootFilesystemConfig,
        Option<vm_config::NativeRootFilesystemConfig>,
    ),
    ClientError,
> {
    let mode = match config.mode.unwrap_or(ConfigRootFilesystemMode::Ephemeral) {
        ConfigRootFilesystemMode::Ephemeral => vm_config::RootFilesystemMode::Ephemeral,
        ConfigRootFilesystemMode::ReadOnly => vm_config::RootFilesystemMode::ReadOnly,
    };
    match config.kind {
        RootFilesystemKind::Overlay => {
            if config.native_plugin.is_some() {
                return Err(ClientError::Sidecar(
                    "rootFilesystem.nativePlugin requires type \"native\"".to_string(),
                ));
            }
            let lowers = config
                .lowers
                .iter()
                .map(serialize_root_lower_config_for_sidecar)
                .collect::<Result<Vec<_>, _>>()?;
            Ok((
                vm_config::RootFilesystemConfig {
                    mode,
                    disable_default_base_layer: config.disable_default_base_layer,
                    lowers,
                    bootstrap_entries: Vec::new(),
                },
                None,
            ))
        }
        RootFilesystemKind::Native => {
            if !config.lowers.is_empty() {
                return Err(ClientError::Sidecar(
                    "native root filesystems do not support rootFilesystem.lowers".to_string(),
                ));
            }
            let plugin = config.native_plugin.as_ref().ok_or_else(|| {
                ClientError::Sidecar(
                    "rootFilesystem.nativePlugin is required for type \"native\"".to_string(),
                )
            })?;
            Ok((
                vm_config::RootFilesystemConfig {
                    mode,
                    disable_default_base_layer: config.disable_default_base_layer,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(vm_config::NativeRootFilesystemConfig {
                    plugin: vm_config::MountPluginDescriptor {
                        id: plugin.id.clone(),
                        config: plugin
                            .config
                            .clone()
                            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
                    },
                    read_only: config.mode == Some(ConfigRootFilesystemMode::ReadOnly),
                }),
            ))
        }
    }
}

fn serialize_root_lower_config_for_sidecar(
    lower: &RootLowerInput,
) -> Result<vm_config::RootFilesystemLowerDescriptor, ClientError> {
    match lower {
        RootLowerInput::BundledBaseFilesystem => {
            Ok(vm_config::RootFilesystemLowerDescriptor::BundledBaseFilesystem)
        }
        RootLowerInput::SnapshotExport(snapshot) => {
            let entries = snapshot
                .source
                .filesystem
                .entries
                .iter()
                .map(serialize_filesystem_entry_config_for_sidecar)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(vm_config::RootFilesystemLowerDescriptor::Snapshot { entries })
        }
    }
}

fn serialize_filesystem_entry_config_for_sidecar(
    entry: &crate::fs::FilesystemEntry,
) -> Result<vm_config::RootFilesystemEntry, ClientError> {
    let mode = u32::from_str_radix(entry.mode.trim_start_matches("0o"), 8).map_err(|error| {
        ClientError::Sidecar(format!(
            "invalid root filesystem mode {} for {}: {error}",
            entry.mode, entry.path
        ))
    })?;
    let kind = match entry.entry_type {
        crate::fs::DirEntryType::File => vm_config::RootFilesystemEntryKind::File,
        crate::fs::DirEntryType::Directory => vm_config::RootFilesystemEntryKind::Directory,
        crate::fs::DirEntryType::Symlink => vm_config::RootFilesystemEntryKind::Symlink,
    };
    let encoding = entry.encoding.map(|encoding| match encoding {
        crate::fs::FilesystemEntryEncoding::Utf8 => vm_config::RootFilesystemEntryEncoding::Utf8,
        crate::fs::FilesystemEntryEncoding::Base64 => {
            vm_config::RootFilesystemEntryEncoding::Base64
        }
    });

    Ok(vm_config::RootFilesystemEntry {
        path: entry.path.clone(),
        kind,
        mode: Some(mode),
        uid: Some(entry.uid),
        gid: Some(entry.gid),
        content: entry.content.clone(),
        encoding,
        target: entry.target.clone(),
        executable: entry.entry_type == crate::fs::DirEntryType::File && (mode & 0o111) != 0,
    })
}

fn serialize_limits_config_for_sidecar(
    limits: Option<&AgentOsLimits>,
) -> Result<Option<vm_config::VmLimitsConfig>, ClientError> {
    let Some(limits) = limits else {
        return Ok(None);
    };
    let value = serde_json::to_value(limits).map_err(|error| {
        ClientError::Sidecar(format!("failed to serialize VM limits config: {error}"))
    })?;
    serde_json::from_value(value).map(Some).map_err(|error| {
        ClientError::Sidecar(format!("failed to encode VM limits config: {error}"))
    })
}

/// Hosts the VM may reach by default (egress). The default network policy is an
/// allowlist of the common hosted LLM provider API endpoints so the standard
/// agent quickstart works with zero network configuration, while still matching
/// the Workers-style default-deny egress model: every other host is denied
/// unless the client widens the `network` permission. Clients opt out by
/// configuring `network` explicitly (e.g. `{ network: "allow" }`).
const DEFAULT_EGRESS_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
    "openrouter.ai",
];

/// Resource patterns for the default egress allowlist. Network permission
/// resources are `dns://<host>` for name resolution and `tcp://<host>:<port>`
/// for the connection itself, so each allowed host needs both forms.
fn default_egress_patterns() -> Vec<String> {
    DEFAULT_EGRESS_HOSTS
        .iter()
        .flat_map(|host| [format!("dns://{host}"), format!("tcp://{host}:*")])
        .collect()
}

/// vm_config variant of the default egress allowlist (deny-by-default rule set).
fn default_network_egress_scope_config() -> vm_config::PatternPermissionScope {
    vm_config::PatternPermissionScope::Rules(vm_config::PatternPermissionRuleSet {
        default: Some(vm_config::PermissionMode::Deny),
        rules: vec![vm_config::PatternPermissionRule {
            mode: vm_config::PermissionMode::Allow,
            operations: vec!["*".to_string()],
            patterns: default_egress_patterns(),
        }],
    })
}

/// Wire variant of the default egress allowlist (deny-by-default rule set).
fn default_network_egress_scope() -> wire::PatternPermissionScope {
    wire::PatternPermissionScope::PatternPermissionRuleSet(wire::PatternPermissionRuleSet {
        default: Some(wire::PermissionMode::Deny),
        rules: vec![wire::PatternPermissionRule {
            mode: wire::PermissionMode::Allow,
            operations: vec!["*".to_string()],
            patterns: default_egress_patterns(),
        }],
    })
}

fn permissions_policy_config(config: &AgentOsConfig) -> vm_config::PermissionsPolicy {
    let Some(permissions) = config.permissions.as_ref() else {
        return default_permissions_policy_config();
    };

    vm_config::PermissionsPolicy {
        fs: Some(
            permissions
                .fs
                .as_ref()
                .map(serialize_fs_permissions_config)
                .unwrap_or(vm_config::FsPermissionScope::Mode(
                    vm_config::PermissionMode::Allow,
                )),
        ),
        network: Some(
            permissions
                .network
                .as_ref()
                .map(serialize_pattern_permissions_config)
                .unwrap_or_else(default_network_egress_scope_config),
        ),
        child_process: Some(
            permissions
                .child_process
                .as_ref()
                .map(serialize_pattern_permissions_config)
                .unwrap_or(vm_config::PatternPermissionScope::Mode(
                    vm_config::PermissionMode::Allow,
                )),
        ),
        process: Some(
            permissions
                .process
                .as_ref()
                .map(serialize_pattern_permissions_config)
                .unwrap_or(vm_config::PatternPermissionScope::Mode(
                    vm_config::PermissionMode::Allow,
                )),
        ),
        env: Some(
            permissions
                .env
                .as_ref()
                .map(serialize_pattern_permissions_config)
                .unwrap_or(vm_config::PatternPermissionScope::Mode(
                    vm_config::PermissionMode::Allow,
                )),
        ),
        binding: Some(
            permissions
                .binding
                .as_ref()
                .map(serialize_pattern_permissions_config)
                .unwrap_or(vm_config::PatternPermissionScope::Mode(
                    vm_config::PermissionMode::Allow,
                )),
        ),
    }
}

/// Default permission policy when the client supplies no `permissions`:
/// allow-all for fs/childProcess/process/env/binding (the VM is itself the
/// isolation boundary), with network egress restricted to the default LLM
/// allowlist (see [`default_network_egress_scope_config`]).
fn default_permissions_policy_config() -> vm_config::PermissionsPolicy {
    vm_config::PermissionsPolicy {
        fs: Some(vm_config::FsPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        network: Some(default_network_egress_scope_config()),
        child_process: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        process: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        env: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        binding: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
    }
}

fn serialize_fs_permissions_config(
    permissions: &crate::config::FsPermissions,
) -> vm_config::FsPermissionScope {
    match permissions {
        crate::config::FsPermissions::Mode(mode) => {
            vm_config::FsPermissionScope::Mode(serialize_permission_mode_config(*mode))
        }
        crate::config::FsPermissions::Rules(rules) => {
            vm_config::FsPermissionScope::Rules(vm_config::FsPermissionRuleSet {
                default: rules.default.map(serialize_permission_mode_config),
                rules: rules
                    .rules
                    .iter()
                    .map(|rule| vm_config::FsPermissionRule {
                        mode: serialize_permission_mode_config(rule.mode),
                        operations: operation_wildcard_if_omitted(&rule.operations),
                        paths: resource_wildcard_if_omitted(&rule.paths),
                    })
                    .collect(),
            })
        }
    }
}

fn serialize_pattern_permissions_config(
    permissions: &crate::config::PatternPermissions,
) -> vm_config::PatternPermissionScope {
    match permissions {
        crate::config::PatternPermissions::Mode(mode) => {
            vm_config::PatternPermissionScope::Mode(serialize_permission_mode_config(*mode))
        }
        crate::config::PatternPermissions::Rules(rules) => {
            vm_config::PatternPermissionScope::Rules(vm_config::PatternPermissionRuleSet {
                default: rules.default.map(serialize_permission_mode_config),
                rules: rules
                    .rules
                    .iter()
                    .map(|rule| vm_config::PatternPermissionRule {
                        mode: serialize_permission_mode_config(rule.mode),
                        operations: operation_wildcard_if_omitted(&rule.operations),
                        patterns: resource_wildcard_if_omitted(&rule.patterns),
                    })
                    .collect(),
            })
        }
    }
}

fn serialize_permission_mode_config(
    mode: crate::config::PermissionMode,
) -> vm_config::PermissionMode {
    match mode {
        crate::config::PermissionMode::Allow => vm_config::PermissionMode::Allow,
        crate::config::PermissionMode::Deny => vm_config::PermissionMode::Deny,
    }
}

/// Await the `ready` VM lifecycle event for `vm_id`, bounded by `timeout_ms`.
async fn wait_for_vm_ready(
    events: &mut broadcast::Receiver<(wire::OwnershipScope, wire::EventPayload)>,
    vm_id: &str,
    timeout_ms: u64,
) -> Result<(), ClientError> {
    let wait = async {
        loop {
            match events.recv().await {
                Ok((ownership, payload)) => match payload {
                    wire::EventPayload::VmLifecycleEvent(event) => {
                        if matches!(event.state, wire::VmLifecycleState::Ready)
                            && wire_ownership_vm_id(&ownership) == Some(vm_id)
                        {
                            return Ok(());
                        }
                    }
                    wire::EventPayload::ProcessOutputEvent(_)
                    | wire::EventPayload::ProcessExitedEvent(_)
                    | wire::EventPayload::ExecutionOutputEvent(_)
                    | wire::EventPayload::ExecutionCompletedEvent(_)
                    | wire::EventPayload::StructuredEvent(_)
                    | wire::EventPayload::ExtEnvelope(_) => {}
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(ClientError::Sidecar(
                        "sidecar transport closed before the VM became ready".to_string(),
                    ));
                }
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(timeout_ms), wait)
        .await
        .map_err(|_| {
            ClientError::Sidecar("timed out waiting for the VM to become ready".to_string())
        })?
}

/// Process-global per-VM binding registry. The shared transport's single host callback routes to
/// the right VM's bindings by frame ownership.
static VM_BINDINGS: OnceCell<SccHashMap<String, Arc<VmBindingRegistry>>> = OnceCell::new();

#[derive(Clone)]
struct VmBindingRegistry {
    bindings: Vec<Bindings>,
    binding_map: HashMap<String, Binding>,
    permissions: Option<Permissions>,
}

fn vm_bindings() -> &'static SccHashMap<String, Arc<VmBindingRegistry>> {
    VM_BINDINGS.get_or_init(SccHashMap::new)
}

/// Process-global map of VM id to client state. The shared ACP host callback
/// uses frame ownership to route filesystem and terminal operations to the
/// correct VM. `Weak` prevents the registry from extending VM lifetime.
static VM_ACP_ROUTERS: OnceCell<SccHashMap<String, Weak<AgentOsInner>>> = OnceCell::new();

fn vm_acp_routers() -> &'static SccHashMap<String, Weak<AgentOsInner>> {
    VM_ACP_ROUTERS.get_or_init(SccHashMap::new)
}

/// Process-global map of sidecar session -> Rust-host js_bridge callback.
///
/// Native root plugins can issue callbacks while `CreateVm` is still in flight, before the client
/// knows the generated VM id. Session ownership is already known by then and stays stable for the VM.
static SESSION_JS_BRIDGE_CALLBACKS: OnceCell<SccHashMap<String, SidecarJsBridgeCallback>> =
    OnceCell::new();

fn session_js_bridge_callbacks() -> &'static SccHashMap<String, SidecarJsBridgeCallback> {
    SESSION_JS_BRIDGE_CALLBACKS.get_or_init(SccHashMap::new)
}

fn sidecar_session_key(connection_id: &str, session_id: &str) -> String {
    format!("{connection_id}\0{session_id}")
}

fn wire_ownership_session_key(ownership: &wire::OwnershipScope) -> Option<String> {
    match ownership {
        wire::OwnershipScope::SessionOwnership(ownership) => Some(sidecar_session_key(
            &ownership.connection_id,
            &ownership.session_id,
        )),
        wire::OwnershipScope::VmOwnership(ownership) => Some(sidecar_session_key(
            &ownership.connection_id,
            &ownership.session_id,
        )),
        wire::OwnershipScope::ConnectionOwnership(_) => None,
    }
}

fn js_bridge_call_callback() -> WireSidecarCallback {
    Arc::new(|payload, ownership| {
        Box::pin(async move {
            let request = match payload {
                wire::SidecarRequestPayload::JsBridgeCallRequest(request) => request,
                wire::SidecarRequestPayload::HostCallbackRequest(_) => {
                    return Ok(wire::SidecarResponsePayload::JsBridgeResultResponse(
                        wire::JsBridgeResultResponse {
                            call_id: "unknown".to_string(),
                            result: None,
                            error: Some(
                                "js-bridge callback received a host callback request".to_string(),
                            ),
                        },
                    ));
                }
                wire::SidecarRequestPayload::ExtEnvelope(_) => {
                    return Ok(wire::SidecarResponsePayload::JsBridgeResultResponse(
                        wire::JsBridgeResultResponse {
                            call_id: "unknown".to_string(),
                            result: None,
                            error: Some(
                                "js-bridge callback received an extension request".to_string(),
                            ),
                        },
                    ));
                }
            };
            Ok(wire::SidecarResponsePayload::JsBridgeResultResponse(
                run_js_bridge_callback(&ownership, request).await,
            ))
        })
    })
}

async fn run_js_bridge_callback(
    ownership: &wire::OwnershipScope,
    request: wire::JsBridgeCallRequest,
) -> wire::JsBridgeResultResponse {
    let call_id = request.call_id;
    let args = match serde_json::from_str::<Value>(&request.args) {
        Ok(args) => args,
        Err(error) => {
            return wire::JsBridgeResultResponse {
                call_id,
                result: None,
                error: Some(format!("Invalid js_bridge args: {error}")),
            };
        }
    };
    let callback = wire_ownership_session_key(ownership)
        .and_then(|key| session_js_bridge_callbacks().read(&key, |_, callback| callback.clone()));
    let Some(callback) = callback else {
        return wire::JsBridgeResultResponse {
            call_id,
            result: None,
            error: Some("No js_bridge callback registered for sidecar session".to_string()),
        };
    };

    let call = SidecarJsBridgeCall {
        call_id: call_id.clone(),
        mount_id: request.mount_id,
        operation: request.operation,
        args,
    };
    match callback(call).await {
        Ok(result) => match result {
            Some(value) => match serde_json::to_string(&value) {
                Ok(result) => wire::JsBridgeResultResponse {
                    call_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => wire::JsBridgeResultResponse {
                    call_id,
                    result: None,
                    error: Some(format!("Invalid js_bridge result: {error}")),
                },
            },
            None => wire::JsBridgeResultResponse {
                call_id,
                result: None,
                error: None,
            },
        },
        Err(error) => wire::JsBridgeResultResponse {
            call_id,
            result: None,
            error: Some(error),
        },
    }
}

/// Transport callback for ACP filesystem and terminal host operations. Durable
/// permission requests are resolved inside the sidecar; the legacy permission
/// callback variant is rejected below.
fn acp_host_callback() -> WireSidecarCallback {
    Arc::new(|payload, ownership| {
        Box::pin(async move {
            match payload {
                wire::SidecarRequestPayload::ExtEnvelope(envelope) => {
                    handle_acp_ext_callback(envelope, &ownership)
                        .await
                        .map_err(|error| TransportError::Sidecar(error.to_string()))
                }
                wire::SidecarRequestPayload::HostCallbackRequest(_)
                | wire::SidecarRequestPayload::JsBridgeCallRequest(_) => Ok(
                    wire::SidecarResponsePayload::ExtEnvelope(wire::ExtEnvelope {
                        namespace: ACP_EXTENSION_NAMESPACE.to_string(),
                        payload: b"ACP callback received a non-extension request".to_vec(),
                    }),
                ),
            }
        })
    })
}

async fn handle_acp_ext_callback(
    envelope: wire::ExtEnvelope,
    ownership: &wire::OwnershipScope,
) -> Result<wire::SidecarResponsePayload, ClientError> {
    if envelope.namespace != ACP_EXTENSION_NAMESPACE {
        return Ok(wire::SidecarResponsePayload::ExtEnvelope(
            wire::ExtEnvelope {
                namespace: envelope.namespace,
                payload: b"unknown extension namespace".to_vec(),
            },
        ));
    }
    let callback: AcpCallback = serde_bare::from_slice(&envelope.payload)
        .map_err(|error| ClientError::Sidecar(format!("invalid ACP callback: {error}")))?;
    let response = match callback {
        AcpCallback::AcpHostRequestCallback(callback) => {
            let response = dispatch_acp_host_request(ownership, &callback.request).await;
            AcpCallbackResponse::AcpHostRequestCallbackResponse(AcpHostRequestCallbackResponse {
                response: Some(response),
            })
        }
    };
    let payload = serde_bare::to_vec(&response).map_err(|error| {
        ClientError::Sidecar(format!("failed to encode ACP callback response: {error}"))
    })?;
    Ok(wire::SidecarResponsePayload::ExtEnvelope(
        wire::ExtEnvelope {
            namespace: ACP_EXTENSION_NAMESPACE.to_string(),
            payload,
        },
    ))
}

// ---------------------------------------------------------------------------
// ACP host-request dispatch (mirrors TS `_dispatchAcpSidecarRequest` ->
// `_handleSupportedAcpSidecarRequest`)
// ---------------------------------------------------------------------------

/// The default `terminal/create` output cap (1 MiB), matching the TS reference.
const ACP_TERMINAL_DEFAULT_OUTPUT_BYTE_LIMIT: usize = 1_048_576;

/// A JSON-RPC error raised while handling an ACP host request. Mirrors the TS `AcpDispatchError`.
struct AcpDispatchError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl AcpDispatchError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            code,
            message: message.into(),
            data: Some(data),
        }
    }
}

impl From<ClientError> for AcpDispatchError {
    fn from(error: ClientError) -> Self {
        match error {
            // Preserve the kernel errno code where one exists (e.g. ENOENT), surfaced through the
            // JSON-RPC `data.code`, while keeping a JSON-RPC internal-error envelope.
            ClientError::Kernel { code, message } => {
                AcpDispatchError::with_data(-32603, message, serde_json::json!({ "code": code }))
            }
            other => AcpDispatchError::new(-32603, other.to_string()),
        }
    }
}

impl From<anyhow::Error> for AcpDispatchError {
    fn from(error: anyhow::Error) -> Self {
        // The filesystem methods return `anyhow::Result`; downcast to recover the kernel errno where
        // the underlying cause is a `ClientError::Kernel` (so e.g. ENOENT survives into `data.code`).
        match error.downcast::<ClientError>() {
            Ok(client_error) => client_error.into(),
            Err(error) => AcpDispatchError::new(-32603, error.to_string()),
        }
    }
}

/// Decode the inbound JSON-RPC request, dispatch it to the matching VM operation, and serialize the
/// JSON-RPC response (success or error). Always returns a valid JSON-RPC response string; the
/// `id`/`error` shape mirrors `_dispatchAcpSidecarRequest`.
async fn dispatch_acp_host_request(ownership: &wire::OwnershipScope, request: &str) -> String {
    let parsed = serde_json::from_str::<Value>(request);
    let (id, method, params_value) = match parsed {
        Ok(value) => {
            let id = value.get("id").cloned().unwrap_or(Value::Null);
            let method = value
                .get("method")
                .and_then(Value::as_str)
                .map(str::to_string);
            (id, method, value.get("params").cloned())
        }
        Err(error) => {
            return acp_error_response(Value::Null, -32700, &format!("Parse error: {error}"), None);
        }
    };

    let Some(method) = method else {
        return acp_error_response(id, -32600, "Invalid Request: missing method", None);
    };

    match handle_acp_host_request(ownership, &method, params_value).await {
        Ok(result) => serde_json::to_string(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .unwrap_or_else(|error| acp_error_response(Value::Null, -32603, &error.to_string(), None)),
        Err(error) => acp_error_response(id, error.code, &error.message, error.data),
    }
}

fn acp_error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> String {
    let mut error = serde_json::json!({
        "code": code,
        "message": message,
    });
    if let Some(data) = data {
        if let Some(map) = error.as_object_mut() {
            map.insert("data".to_string(), data);
        }
    }
    serde_json::to_string(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    }))
    .unwrap_or_else(|_| {
        String::from(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"failed to encode error response"}}"#)
    })
}

/// Resolve the `AgentOs` that owns the VM named in `ownership`, mirroring `route_permission_request`.
fn resolve_acp_agent(ownership: &wire::OwnershipScope) -> Result<AgentOs, AcpDispatchError> {
    let vm_id = wire_ownership_vm_id(ownership).unwrap_or("");
    let inner = vm_acp_routers()
        .read(vm_id, |_, weak| weak.clone())
        .and_then(|weak| weak.upgrade());
    inner
        .map(|inner| AgentOs { inner })
        .ok_or_else(|| AcpDispatchError::new(-32603, "VM is no longer available"))
}

/// Mirror of TS `_handleSupportedAcpSidecarRequest`: dispatch the JSON-RPC method to the matching VM
/// operation. Returns the JSON-RPC `result` value on success.
async fn handle_acp_host_request(
    ownership: &wire::OwnershipScope,
    method: &str,
    params_value: Option<Value>,
) -> Result<Value, AcpDispatchError> {
    let params = acp_params(method, params_value)?;
    match method {
        "fs/read" | "fs/read_text_file" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_read_file(&agent, &params).await
        }
        "fs/write" | "fs/write_text_file" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_write_file(&agent, &params).await
        }
        "fs/readDir" | "fs/read_dir" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_read_dir(&agent, &params).await
        }
        "terminal/create" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_create_terminal(&agent, &params)
        }
        "terminal/write" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_write_terminal(&agent, &params)
        }
        "terminal/output" | "terminal/read" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_read_terminal(&agent, &params)
        }
        "terminal/wait_for_exit" | "terminal/waitForExit" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_wait_for_terminal_exit(&agent, &params).await
        }
        "terminal/kill" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_kill_terminal(&agent, &params)
        }
        "terminal/release" | "terminal/close" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_release_terminal(&agent, &params)
        }
        "terminal/resize" => {
            let agent = resolve_acp_agent(ownership)?;
            handle_acp_resize_terminal(&agent, &params)
        }
        other => Err(AcpDispatchError::with_data(
            -32601,
            format!("Method not found: {other}"),
            serde_json::json!({ "method": other }),
        )),
    }
}

// --- ACP host-request param helpers (mirror TS `_acpParams` / `_require*` / `_optional*`) ---

fn acp_params(
    method: &str,
    params_value: Option<Value>,
) -> Result<Map<String, Value>, AcpDispatchError> {
    match params_value {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map),
        Some(_) => Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires object params"),
        )),
    }
}

fn require_acp_string(
    params: &Map<String, Value>,
    name: &str,
    method: &str,
) -> Result<String, AcpDispatchError> {
    match params.get(name).and_then(Value::as_str) {
        Some(value) => Ok(value.to_string()),
        None => Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires a string {name}"),
        )),
    }
}

fn optional_acp_string(
    params: &Map<String, Value>,
    name: &str,
    method: &str,
) -> Result<Option<String>, AcpDispatchError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires {name} to be a string when provided"),
        )),
    }
}

fn optional_acp_number(
    params: &Map<String, Value>,
    name: &str,
    method: &str,
) -> Result<Option<f64>, AcpDispatchError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => match value.as_f64() {
            Some(number) if number.is_finite() => Ok(Some(number)),
            _ => Err(AcpDispatchError::new(
                -32602,
                format!("{method} requires {name} to be a number when provided"),
            )),
        },
    }
}

fn optional_acp_string_array(
    params: &Map<String, Value>,
    name: &str,
    method: &str,
) -> Result<Option<Vec<String>>, AcpDispatchError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(value) => out.push(value.to_string()),
                    None => {
                        return Err(AcpDispatchError::new(
                            -32602,
                            format!(
                                "{method} requires {name} to be an array of strings when provided"
                            ),
                        ))
                    }
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires {name} to be an array of strings when provided"),
        )),
    }
}

/// Parse the ACP `env` param, accepting either an object map or a `[{ name, value }]` array, matching
/// the TS `_optionalAcpEnvParam`.
fn optional_acp_env(
    params: &Map<String, Value>,
    name: &str,
    method: &str,
) -> Result<Option<BTreeMap<String, String>>, AcpDispatchError> {
    match params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut env = BTreeMap::new();
            for entry in items {
                let Some(record) = entry.as_object() else {
                    return Err(AcpDispatchError::new(
                        -32602,
                        format!("{method} requires {name} entries to be {{ name, value }} objects"),
                    ));
                };
                match (
                    record.get("name").and_then(Value::as_str),
                    record.get("value").and_then(Value::as_str),
                ) {
                    (Some(key), Some(value)) => {
                        env.insert(key.to_string(), value.to_string());
                    }
                    _ => {
                        return Err(AcpDispatchError::new(
                            -32602,
                            format!(
                                "{method} requires {name} entries to be {{ name, value }} objects"
                            ),
                        ))
                    }
                }
            }
            Ok(Some(env))
        }
        Some(Value::Object(map)) => {
            let mut env = BTreeMap::new();
            for (key, value) in map {
                match value.as_str() {
                    Some(value) => {
                        env.insert(key.clone(), value.to_string());
                    }
                    None => {
                        return Err(AcpDispatchError::new(
                            -32602,
                            format!("{method} requires {name} values to be strings"),
                        ))
                    }
                }
            }
            Ok(Some(env))
        }
        Some(_) => Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires {name} to be an object or name/value array"),
        )),
    }
}

// --- fs/* handlers ---

async fn handle_acp_read_file(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "fs/read";
    let path = require_acp_string(params, "path", method)?;
    let line = optional_acp_number(params, "line", method)?;
    let limit = optional_acp_number(params, "limit", method)?;
    let encoding = optional_acp_string(params, "encoding", method)?;
    let bytes = agent.read_file(&path).await?;
    if encoding.as_deref() == Some("base64") {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        return Ok(serde_json::json!({ "content": BASE64.encode(&bytes) }));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if line.is_none() && limit.is_none() {
        return Ok(serde_json::json!({ "content": text }));
    }
    let start_line = line.map(|n| n.trunc() as i64).unwrap_or(1).max(1);
    let lines: Vec<&str> = text.split('\n').collect();
    let start_index = (start_line - 1).max(0) as usize;
    let selected: Vec<&str> = match limit {
        None => lines.into_iter().skip(start_index).collect(),
        Some(limit) => {
            let limit = limit.trunc().max(0.0) as usize;
            lines.into_iter().skip(start_index).take(limit).collect()
        }
    };
    Ok(serde_json::json!({ "content": selected.join("\n") }))
}

async fn handle_acp_write_file(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "fs/write";
    let path = require_acp_string(params, "path", method)?;
    let content = require_acp_string(params, "content", method)?;
    let encoding = optional_acp_string(params, "encoding", method)?;
    if encoding.as_deref() == Some("base64") {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        let decoded = BASE64.decode(content.as_bytes()).map_err(|error| {
            AcpDispatchError::new(
                -32602,
                format!("{method} content is not valid base64: {error}"),
            )
        })?;
        agent.write_file(&path, decoded).await?;
    } else {
        agent.write_file(&path, content).await?;
    }
    Ok(Value::Null)
}

async fn handle_acp_read_dir(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "fs/readDir";
    let path = require_acp_string(params, "path", method)?;
    let entries = agent.acp_read_dir_with_types(&path).await?;
    let mapped: Vec<Value> = entries
        .into_iter()
        .map(|entry| {
            let child_path = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{path}/{}", entry.name)
            };
            let entry_type = if entry.is_symbolic_link {
                "symlink"
            } else if entry.is_directory {
                "directory"
            } else {
                "file"
            };
            serde_json::json!({
                "name": entry.name,
                "path": child_path,
                "type": entry_type,
            })
        })
        .collect();
    Ok(serde_json::json!({ "entries": mapped }))
}

// --- terminal/* handlers ---

fn require_acp_terminal_id(
    params: &Map<String, Value>,
    method: &str,
) -> Result<String, AcpDispatchError> {
    require_acp_string(params, "terminalId", method)
}

fn handle_acp_create_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/create";
    let command = require_acp_string(params, "command", method)?;
    let args = optional_acp_string_array(params, "args", method)?;
    let env = optional_acp_env(params, "env", method)?;
    let cwd = optional_acp_string(params, "cwd", method)?;
    let cols = optional_acp_number(params, "cols", method)?;
    let rows = optional_acp_number(params, "rows", method)?;
    let output_byte_limit = optional_acp_number(params, "outputByteLimit", method)?
        .map(|n| n.trunc().max(0.0) as usize)
        .unwrap_or(ACP_TERMINAL_DEFAULT_OUTPUT_BYTE_LIMIT);

    let counter = agent
        .inner()
        .host_acp_terminal_counter
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let terminal_id = format!("acp-terminal-{counter}");

    let output = Arc::new(parking_lot::Mutex::new(HostAcpTerminalOutput {
        buffer: String::new(),
        truncated: false,
        output_byte_limit,
    }));
    let (exit_tx, exit_rx) = watch::channel::<Option<i32>>(None);

    // Build the PTY shell. Both stdout and stderr are appended to the same output buffer, mirroring
    // the TS handle where `onData` and `onStderr` both append to `terminal.output`.
    let mut shell_options = crate::shell::OpenShellOptions {
        command: Some(command),
        cwd,
        ..Default::default()
    };
    if let Some(args) = args {
        shell_options.args = args;
    }
    if let Some(env) = env {
        shell_options.env = env;
    }
    if let Some(cols) = cols {
        shell_options.cols = Some(cols.trunc() as u16);
    }
    if let Some(rows) = rows {
        shell_options.rows = Some(rows.trunc() as u16);
    }
    // Both stdout and stderr are appended to the single combined output buffer inside
    // `acp_open_terminal`'s fan-out task (mirroring the TS handle's `onData`/`onStderr`).
    let buffer_sink = output.clone();
    let handle = agent
        .acp_open_terminal(shell_options, exit_tx, move |data: &[u8]| {
            append_acp_terminal_output(&buffer_sink, data);
        })
        .map_err(|error| AcpDispatchError::new(-32603, error.to_string()))?;
    let shell_id = handle.shell_id.clone();

    let entry = HostAcpTerminal {
        shell_id,
        output,
        exit_rx,
    };
    if agent
        .inner()
        .host_acp_terminals
        .insert(terminal_id.clone(), entry)
        .is_err()
    {
        return Err(AcpDispatchError::new(
            -32603,
            format!("ACP terminal id collision: {terminal_id}"),
        ));
    }

    Ok(serde_json::json!({ "terminalId": terminal_id }))
}

fn append_acp_terminal_output(
    output: &Arc<parking_lot::Mutex<HostAcpTerminalOutput>>,
    data: &[u8],
) {
    let chunk = String::from_utf8_lossy(data);
    if chunk.is_empty() {
        return;
    }
    let mut state = output.lock();
    state.buffer.push_str(&chunk);
    let limit = state.output_byte_limit;
    if state.buffer.len() > limit {
        // Trim from the front to the limit, on a char boundary, matching the TS slice-to-limit
        // behavior (which trims to the last `limit` UTF-16 code units; bytes are an acceptable port).
        let overflow = state.buffer.len() - limit;
        let mut cut = overflow;
        while cut < state.buffer.len() && !state.buffer.is_char_boundary(cut) {
            cut += 1;
        }
        state.buffer = state.buffer.split_off(cut);
        state.truncated = true;
    }
}

fn handle_acp_write_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/write";
    let terminal_id = require_acp_terminal_id(params, method)?;
    let shell_id = acp_terminal_shell_id(agent, &terminal_id)?;
    let data = require_acp_string(params, "data", method)?;
    let encoding = optional_acp_string(params, "encoding", method)?;
    let input = if encoding.as_deref() == Some("base64") {
        use base64::engine::general_purpose::STANDARD as BASE64;
        use base64::Engine as _;
        let decoded = BASE64.decode(data.as_bytes()).map_err(|error| {
            AcpDispatchError::new(
                -32602,
                format!("{method} data is not valid base64: {error}"),
            )
        })?;
        crate::process::StdinInput::Bytes(decoded)
    } else {
        crate::process::StdinInput::Text(data)
    };
    agent
        .write_shell(&shell_id, input)
        .map_err(|error| AcpDispatchError::new(-32603, error.to_string()))?;
    Ok(Value::Null)
}

fn handle_acp_read_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/output";
    let terminal_id = require_acp_terminal_id(params, method)?;
    agent
        .inner()
        .host_acp_terminals
        .read(&terminal_id, |_, terminal| {
            let (output, truncated) = {
                let state = terminal.output.lock();
                (state.buffer.clone(), state.truncated)
            };
            let mut result = serde_json::json!({
                "output": output,
                "truncated": truncated,
            });
            if let Some(exit_code) = *terminal.exit_rx.borrow() {
                if let Some(map) = result.as_object_mut() {
                    map.insert(
                        "exitStatus".to_string(),
                        serde_json::json!({ "exitCode": exit_code, "signal": Value::Null }),
                    );
                }
            }
            result
        })
        .ok_or_else(|| {
            AcpDispatchError::new(-32602, format!("ACP terminal not found: {terminal_id}"))
        })
}

async fn handle_acp_wait_for_terminal_exit(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/wait_for_exit";
    let terminal_id = require_acp_terminal_id(params, method)?;
    let mut exit_rx = agent
        .inner()
        .host_acp_terminals
        .read(&terminal_id, |_, terminal| terminal.exit_rx.clone())
        .ok_or_else(|| {
            AcpDispatchError::new(-32602, format!("ACP terminal not found: {terminal_id}"))
        })?;
    let exit_code = loop {
        if let Some(code) = *exit_rx.borrow() {
            break code;
        }
        if exit_rx.changed().await.is_err() {
            // Sender dropped (terminal released / VM disposed) without a recorded
            // exit code. Surface that as an abnormal exit instead of pretending
            // the terminal completed cleanly with exit 0.
            break exit_rx.borrow().unwrap_or(1);
        }
    };
    Ok(serde_json::json!({ "exitCode": exit_code, "signal": Value::Null }))
}

fn handle_acp_kill_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/kill";
    let terminal_id = require_acp_terminal_id(params, method)?;
    let shell_id = acp_terminal_shell_id(agent, &terminal_id)?;
    // The native shell API only exposes SIGTERM teardown via `close_shell`'s kill; the explicit
    // `signal` param is accepted for parity but the underlying kill is fixed to SIGTERM. The terminal
    // entry is retained (matching TS `kill`, which does not delete the terminal) so `terminal/output`
    // and `terminal/wait_for_exit` still work afterward.
    agent
        .acp_kill_terminal_shell(&shell_id)
        .map_err(|error| AcpDispatchError::new(-32603, error.to_string()))?;
    Ok(Value::Null)
}

fn handle_acp_release_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/release";
    let terminal_id = require_acp_terminal_id(params, method)?;
    let Some((_, terminal)) = agent.inner().host_acp_terminals.remove(&terminal_id) else {
        return Err(AcpDispatchError::new(
            -32602,
            format!("ACP terminal not found: {terminal_id}"),
        ));
    };
    // If the process has not exited yet, kill it (TS releases by killing when `exitCode === null`).
    if terminal.exit_rx.borrow().is_none() {
        let _ = agent.acp_kill_terminal_shell(&terminal.shell_id);
    }
    // Closing the shell removes the registry entry and ends the fan-out/exit task naturally.
    let _ = agent.close_shell(&terminal.shell_id);
    Ok(Value::Null)
}

fn handle_acp_resize_terminal(
    agent: &AgentOs,
    params: &Map<String, Value>,
) -> Result<Value, AcpDispatchError> {
    let method = "terminal/resize";
    let terminal_id = require_acp_terminal_id(params, method)?;
    let shell_id = acp_terminal_shell_id(agent, &terminal_id)?;
    let cols = optional_acp_number(params, "cols", method)?;
    let rows = optional_acp_number(params, "rows", method)?;
    let (Some(cols), Some(rows)) = (cols, rows) else {
        return Err(AcpDispatchError::new(
            -32602,
            format!("{method} requires numeric cols and rows"),
        ));
    };
    agent
        .resize_shell(&shell_id, cols.trunc() as u16, rows.trunc() as u16)
        .map_err(|error| AcpDispatchError::new(-32603, error.to_string()))?;
    Ok(Value::Null)
}

/// Look up the backing shell id for a host-request terminal, or a JSON-RPC -32602 error.
fn acp_terminal_shell_id(agent: &AgentOs, terminal_id: &str) -> Result<String, AcpDispatchError> {
    agent
        .inner()
        .host_acp_terminals
        .read(terminal_id, |_, terminal| terminal.shell_id.clone())
        .ok_or_else(|| {
            AcpDispatchError::new(-32602, format!("ACP terminal not found: {terminal_id}"))
        })
}

/// The transport callback that answers guest binding invocations by running the matching host binding.
fn host_callback_callback() -> WireSidecarCallback {
    Arc::new(|payload, ownership| {
        Box::pin(async move {
            let request = match payload {
                wire::SidecarRequestPayload::HostCallbackRequest(request) => request,
                wire::SidecarRequestPayload::JsBridgeCallRequest(_) => {
                    return Ok(wire::SidecarResponsePayload::HostCallbackResultResponse(
                        wire::HostCallbackResultResponse {
                            invocation_id: "unknown".to_string(),
                            result: None,
                            error: Some("host-callback received a non-binding request".to_string()),
                        },
                    ));
                }
                wire::SidecarRequestPayload::ExtEnvelope(envelope) => {
                    return Ok(wire::SidecarResponsePayload::ExtEnvelope(
                        wire::ExtEnvelope {
                            namespace: envelope.namespace,
                            payload: b"host-callback received an extension request".to_vec(),
                        },
                    ));
                }
            };
            Ok(wire::SidecarResponsePayload::HostCallbackResultResponse(
                run_host_callback(&ownership, request).await,
            ))
        })
    })
}

/// Run a single binding invocation against the per-VM host-binding registry, honoring the timeout. Mirrors
/// TS `handleHostCallback` (unknown-binding + timeout + error shapes).
async fn run_host_callback(
    ownership: &wire::OwnershipScope,
    request: wire::HostCallbackRequest,
) -> wire::HostCallbackResultResponse {
    let input = match serde_json::from_str::<Value>(&request.input) {
        Ok(input) => input,
        Err(error) => {
            return wire::HostCallbackResultResponse {
                invocation_id: request.invocation_id,
                result: None,
                error: Some(format!("Invalid host callback input: {error}")),
            };
        }
    };
    let vm_id = wire_ownership_vm_id(ownership).unwrap_or("");
    let registry = vm_bindings().read(vm_id, |_, registry| registry.clone());
    let Some(registry) = registry else {
        return wire::HostCallbackResultResponse {
            invocation_id: request.invocation_id,
            result: None,
            error: Some(format!("Unknown binding \"{}\"", request.callback_key)),
        };
    };

    if let Some(command) = parse_host_command_callback_input(&input) {
        return match run_host_command_callback(ownership, registry.as_ref(), command).await {
            Ok(value) => match host_callback_json_result(value) {
                Ok(result) => wire::HostCallbackResultResponse {
                    invocation_id: request.invocation_id,
                    result: Some(result),
                    error: None,
                },
                Err(error) => wire::HostCallbackResultResponse {
                    invocation_id: request.invocation_id,
                    result: None,
                    error: Some(error),
                },
            },
            Err(error) => wire::HostCallbackResultResponse {
                invocation_id: request.invocation_id,
                result: None,
                error: Some(error),
            },
        };
    }

    let binding = registry.binding_map.get(&request.callback_key).cloned();
    let Some(binding) = binding else {
        return wire::HostCallbackResultResponse {
            invocation_id: request.invocation_id,
            result: None,
            error: Some(format!("Unknown binding \"{}\"", request.callback_key)),
        };
    };
    let timeout = Duration::from_millis(request.timeout_ms.max(1));
    match tokio::time::timeout(timeout, (binding.execute)(input)).await {
        Ok(Ok(value)) => match host_callback_json_result(value) {
            Ok(result) => wire::HostCallbackResultResponse {
                invocation_id: request.invocation_id,
                result: Some(result),
                error: None,
            },
            Err(error) => wire::HostCallbackResultResponse {
                invocation_id: request.invocation_id,
                result: None,
                error: Some(error),
            },
        },
        Ok(Err(error)) => wire::HostCallbackResultResponse {
            invocation_id: request.invocation_id,
            result: None,
            error: Some(error),
        },
        Err(_) => wire::HostCallbackResultResponse {
            invocation_id: request.invocation_id,
            result: None,
            error: Some(format!(
                "Binding \"{}\" timed out after {}ms",
                request.callback_key, request.timeout_ms
            )),
        },
    }
}

#[derive(Debug, Deserialize)]
struct HostCommandCallbackInput {
    #[serde(rename = "type")]
    kind: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    cwd: String,
}

fn parse_host_command_callback_input(input: &Value) -> Option<HostCommandCallbackInput> {
    let command = serde_json::from_value::<HostCommandCallbackInput>(input.clone()).ok()?;
    if command.kind == "command" {
        Some(command)
    } else {
        None
    }
}

async fn run_host_command_callback(
    ownership: &wire::OwnershipScope,
    registry: &VmBindingRegistry,
    command: HostCommandCallbackInput,
) -> Result<Value, String> {
    if command.command == "agentos" {
        return handle_agentos_registry_command(ownership, registry, &command).await;
    }
    let Some(collection) = registry
        .bindings
        .iter()
        .find(|collection| format!("agentos-{}", collection.name) == command.command)
    else {
        return Err(format!(
            "Unknown host callback command \"{}\"",
            command.command
        ));
    };
    handle_agentos_binding_command(ownership, registry, &command, collection).await
}

async fn handle_agentos_registry_command(
    ownership: &wire::OwnershipScope,
    registry: &VmBindingRegistry,
    command: &HostCommandCallbackInput,
) -> Result<Value, String> {
    let Some(subcommand) = command.args.first() else {
        return Ok(json_object([(
            "usage",
            Value::String(String::from(
                "agentos <command>: list-bindings [collection], <collection> --help, or <collection> <binding> ...",
            )),
        )]));
    };
    if is_help_flag(subcommand) {
        return Ok(json_object([(
            "usage",
            Value::String(String::from(
                "agentos <command>: list-bindings [collection], <collection> --help, or <collection> <binding> ...",
            )),
        )]));
    }
    if subcommand == "list-bindings" {
        return match command.args.get(1) {
            Some(collection_name) => describe_bindings_payload(&registry.bindings, collection_name),
            None => Ok(list_bindings_payload(&registry.bindings)),
        };
    }

    let Some(collection) = registry
        .bindings
        .iter()
        .find(|collection| collection.name == *subcommand)
    else {
        return Err(format!(
            "No collection \"{subcommand}\". Available: {}",
            bindings_names(&registry.bindings)
        ));
    };

    let Some(binding_name) = command.args.get(1) else {
        return describe_bindings_payload(&registry.bindings, subcommand);
    };
    if is_help_flag(binding_name) {
        return describe_bindings_payload(&registry.bindings, subcommand);
    }
    if command.args.get(2).is_some_and(|value| is_help_flag(value)) {
        return describe_binding_payload(collection, binding_name);
    }
    invoke_binding(
        ownership,
        registry,
        collection,
        binding_name,
        command.args.get(2..).unwrap_or_default(),
        &command.cwd,
    )
    .await
}

async fn handle_agentos_binding_command(
    ownership: &wire::OwnershipScope,
    registry: &VmBindingRegistry,
    command: &HostCommandCallbackInput,
    collection: &Bindings,
) -> Result<Value, String> {
    let Some(binding_name) = command.args.first() else {
        return describe_bindings_payload(&registry.bindings, &collection.name);
    };
    if is_help_flag(binding_name) {
        return describe_bindings_payload(&registry.bindings, &collection.name);
    }
    if command.args.get(1).is_some_and(|value| is_help_flag(value)) {
        return describe_binding_payload(collection, binding_name);
    }
    invoke_binding(
        ownership,
        registry,
        collection,
        binding_name,
        command.args.get(1..).unwrap_or_default(),
        &command.cwd,
    )
    .await
}

async fn invoke_binding(
    ownership: &wire::OwnershipScope,
    registry: &VmBindingRegistry,
    collection: &Bindings,
    binding_name: &str,
    args: &[String],
    cwd: &str,
) -> Result<Value, String> {
    let callback_key = format!("{}:{binding_name}", collection.name);
    let Some(binding) = registry.binding_map.get(&callback_key).cloned() else {
        return Err(format!(
            "No binding \"{binding_name}\" in collection \"{}\". Available: {}",
            collection.name,
            binding_names(collection)
        ));
    };

    if binding_permission_mode(registry.permissions.as_ref(), &callback_key)
        != PermissionMode::Allow
    {
        return Err(format!(
            "EACCES: blocked by binding.invoke policy for {callback_key}"
        ));
    }

    let input = parse_binding_input(ownership, &binding, args, cwd).await?;
    validate_binding_input(&binding.input_schema, &input).map_err(|error| error.to_string())?;

    let timeout = Duration::from_millis(binding.timeout_ms.unwrap_or(30_000).max(1));
    match tokio::time::timeout(timeout, (binding.execute)(input)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(format!(
            "Binding \"{callback_key}\" timed out after {}ms",
            binding.timeout_ms.unwrap_or(30_000)
        )),
    }
}

async fn parse_binding_input(
    ownership: &wire::OwnershipScope,
    binding: &Binding,
    args: &[String],
    cwd: &str,
) -> Result<Value, String> {
    if args.first().is_some_and(|arg| arg == "--json") {
        let value = args
            .get(1)
            .ok_or_else(|| String::from("Flag --json requires a value"))?;
        return serde_json::from_str(value)
            .map_err(|error| format!("Invalid JSON for --json: {error}"));
    }

    if args.first().is_some_and(|arg| arg == "--json-file") {
        let path = args
            .get(1)
            .ok_or_else(|| String::from("Flag --json-file requires a value"))?;
        let guest_path = normalize_guest_path(if path.starts_with('/') {
            path.clone()
        } else {
            format!("{cwd}/{path}")
        });
        let vm_id = wire_ownership_vm_id(ownership).unwrap_or("");
        let inner = vm_acp_routers()
            .read(vm_id, |_, weak| weak.clone())
            .and_then(|weak| weak.upgrade())
            .ok_or_else(|| String::from("Invalid JSON file: VM is no longer available"))?;
        let bytes = AgentOs { inner }
            .read_file(&guest_path)
            .await
            .map_err(|error| format!("Invalid JSON file: {error}"))?;
        let text =
            String::from_utf8(bytes).map_err(|error| format!("Invalid JSON file: {error}"))?;
        return serde_json::from_str(&text).map_err(|error| format!("Invalid JSON file: {error}"));
    }

    parse_binding_argv(&binding.input_schema, args)
}

fn host_callback_json_result(value: Value) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|error| format!("Invalid host callback result: {error}"))
}

fn parse_binding_argv(schema: &Value, argv: &[String]) -> Result<Value, String> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();

    let mut flag_to_field = BTreeMap::new();
    for (field_name, field_schema) in &properties {
        flag_to_field.insert(
            camel_to_kebab(field_name),
            (field_name.clone(), field_schema.clone()),
        );
    }

    let mut input = Map::new();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if !arg.starts_with("--") {
            return Err(format!("Unexpected positional argument: \"{arg}\""));
        }

        let raw_flag = &arg[2..];
        let (flag_name, negated) = raw_flag
            .strip_prefix("no-")
            .map(|name| (name, true))
            .unwrap_or((raw_flag, false));
        let Some((field_name, field_schema)) = flag_to_field.get(flag_name) else {
            return Err(format!("Unknown flag: --{raw_flag}"));
        };
        let field_type = json_schema_type(field_schema);

        if negated {
            if field_type != Some("boolean") {
                return Err(format!("Unknown flag: --{raw_flag}"));
            }
            input.insert(field_name.clone(), Value::Bool(false));
            index += 1;
            continue;
        }

        match field_type {
            Some("boolean") => {
                input.insert(field_name.clone(), Value::Bool(true));
                index += 1;
            }
            Some("number") | Some("integer") => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                let number = value
                    .parse::<f64>()
                    .map_err(|_| format!("Flag --{raw_flag} expects a number, got \"{value}\""))?;
                let number = serde_json::Number::from_f64(number).ok_or_else(|| {
                    format!("Flag --{raw_flag} expects a finite number, got \"{value}\"")
                })?;
                input.insert(field_name.clone(), Value::Number(number));
                index += 2;
            }
            Some("array") => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                let item_type = field_schema.get("items").and_then(json_schema_type);
                let parsed_value = match item_type {
                    Some("number") | Some("integer") => {
                        let number = value.parse::<f64>().map_err(|_| {
                            format!("Flag --{raw_flag} expects a number value, got \"{value}\"")
                        })?;
                        let number = serde_json::Number::from_f64(number).ok_or_else(|| {
                            format!(
                                "Flag --{raw_flag} expects a finite number value, got \"{value}\""
                            )
                        })?;
                        Value::Number(number)
                    }
                    Some("boolean") => {
                        let boolean = value.parse::<bool>().map_err(|_| {
                            format!("Flag --{raw_flag} expects a boolean value, got \"{value}\"")
                        })?;
                        Value::Bool(boolean)
                    }
                    _ => Value::String(value.clone()),
                };
                input
                    .entry(field_name.clone())
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
                    .expect("array field should always contain an array")
                    .push(parsed_value);
                index += 2;
            }
            _ => {
                let value = argv
                    .get(index + 1)
                    .ok_or_else(|| format!("Flag --{raw_flag} requires a value"))?;
                input.insert(field_name.clone(), Value::String(value.clone()));
                index += 2;
            }
        }
    }

    for field_name in required {
        if !input.contains_key(&field_name) {
            return Err(format!(
                "Missing required flag: --{}",
                camel_to_kebab(&field_name)
            ));
        }
    }

    Ok(Value::Object(input))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindingInputSchemaViolation {
    path: String,
    expected: String,
    actual: String,
}

impl BindingInputSchemaViolation {
    fn new(
        path: impl Into<String>,
        expected: impl Into<String>,
        actual: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            expected: expected.into(),
            actual: actual.into(),
        }
    }
}

impl std::fmt::Display for BindingInputSchemaViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "BindingInputSchemaViolation at {}: expected {}, got {}",
            self.path, self.expected, self.actual
        )
    }
}

fn validate_binding_input(
    schema: &Value,
    input: &Value,
) -> Result<(), BindingInputSchemaViolation> {
    validate_binding_input_at_path(schema, input, "$")
}

fn validate_binding_input_at_path(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), BindingInputSchemaViolation> {
    if schema.is_null() || schema.as_object().is_some_and(|object| object.is_empty()) {
        return Ok(());
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        return validate_schema_branches(branches, input, path, "anyOf");
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        return validate_schema_branches(branches, input, path, "oneOf");
    }
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        if enum_values.iter().any(|candidate| candidate == input) {
            return Ok(());
        }
        return Err(BindingInputSchemaViolation::new(
            path,
            format!(
                "one of {}",
                enum_values
                    .iter()
                    .map(compact_json)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            describe_value(input),
        ));
    }
    if let Some(expected) = schema.get("const") {
        if expected == input {
            return Ok(());
        }
        return Err(BindingInputSchemaViolation::new(
            path,
            format!("constant {}", compact_json(expected)),
            describe_value(input),
        ));
    }

    match schema.get("type") {
        Some(Value::String(expected_type)) => {
            validate_typed_binding_input(schema, input, path, expected_type)
        }
        Some(Value::Array(expected_types)) => {
            let mut first_error = None;
            for expected_type in expected_types.iter().filter_map(Value::as_str) {
                match validate_typed_binding_input(schema, input, path, expected_type) {
                    Ok(()) => return Ok(()),
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
            }
            Err(first_error.unwrap_or_else(|| {
                BindingInputSchemaViolation::new(
                    path,
                    describe_expected(schema),
                    describe_value(input),
                )
            }))
        }
        Some(_) => Ok(()),
        None if has_object_keywords(schema) => {
            validate_typed_binding_input(schema, input, path, "object")
        }
        None => Ok(()),
    }
}

fn validate_schema_branches(
    branches: &[Value],
    input: &Value,
    path: &str,
    keyword: &str,
) -> Result<(), BindingInputSchemaViolation> {
    let mut first_error = None;
    for branch in branches {
        match validate_binding_input_at_path(branch, input, path) {
            Ok(()) => return Ok(()),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| {
        BindingInputSchemaViolation::new(
            path,
            format!(
                "{keyword} branch ({})",
                branches
                    .iter()
                    .map(describe_expected)
                    .collect::<Vec<_>>()
                    .join(" | ")
            ),
            describe_value(input),
        )
    }))
}

fn validate_typed_binding_input(
    schema: &Value,
    input: &Value,
    path: &str,
    expected_type: &str,
) -> Result<(), BindingInputSchemaViolation> {
    match expected_type {
        "null" if input.is_null() => Ok(()),
        "null" => Err(type_violation(path, expected_type, input)),
        "boolean" if input.is_boolean() => Ok(()),
        "boolean" => Err(type_violation(path, expected_type, input)),
        "string" => validate_string_binding_input(schema, input, path),
        "number" => validate_number_binding_input(schema, input, path, false),
        "integer" => validate_number_binding_input(schema, input, path, true),
        "array" => validate_array_binding_input(schema, input, path),
        "object" => validate_object_binding_input(schema, input, path),
        _ => Ok(()),
    }
}

fn validate_string_binding_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), BindingInputSchemaViolation> {
    let Some(value) = input.as_str() else {
        return Err(type_violation(path, "string", input));
    };
    if let Some(min_length) = schema.get("minLength").and_then(Value::as_u64) {
        if value.chars().count() < min_length as usize {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!("string with minLength {min_length}"),
                format!("string length {}", value.chars().count()),
            ));
        }
    }
    if let Some(max_length) = schema.get("maxLength").and_then(Value::as_u64) {
        if value.chars().count() > max_length as usize {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!("string with maxLength {max_length}"),
                format!("string length {}", value.chars().count()),
            ));
        }
    }
    Ok(())
}

fn validate_number_binding_input(
    schema: &Value,
    input: &Value,
    path: &str,
    expect_integer: bool,
) -> Result<(), BindingInputSchemaViolation> {
    let Some(number) = input.as_f64() else {
        return Err(type_violation(
            path,
            if expect_integer { "integer" } else { "number" },
            input,
        ));
    };
    if expect_integer && number.fract() != 0.0 {
        return Err(type_violation(path, "integer", input));
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_f64) {
        if number < minimum {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!(
                    "{} >= {}",
                    if expect_integer { "integer" } else { "number" },
                    minimum
                ),
                compact_json(input),
            ));
        }
    }
    if let Some(minimum) = schema.get("exclusiveMinimum").and_then(Value::as_f64) {
        if number <= minimum {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!(
                    "{} > {}",
                    if expect_integer { "integer" } else { "number" },
                    minimum
                ),
                compact_json(input),
            ));
        }
    }
    if let Some(maximum) = schema.get("maximum").and_then(Value::as_f64) {
        if number > maximum {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!(
                    "{} <= {}",
                    if expect_integer { "integer" } else { "number" },
                    maximum
                ),
                compact_json(input),
            ));
        }
    }
    if let Some(maximum) = schema.get("exclusiveMaximum").and_then(Value::as_f64) {
        if number >= maximum {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!(
                    "{} < {}",
                    if expect_integer { "integer" } else { "number" },
                    maximum
                ),
                compact_json(input),
            ));
        }
    }
    Ok(())
}

fn validate_array_binding_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), BindingInputSchemaViolation> {
    let Some(items) = input.as_array() else {
        return Err(type_violation(path, "array", input));
    };
    if let Some(min_items) = schema.get("minItems").and_then(Value::as_u64) {
        if items.len() < min_items as usize {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!("array with minItems {min_items}"),
                format!("array length {}", items.len()),
            ));
        }
    }
    if let Some(max_items) = schema.get("maxItems").and_then(Value::as_u64) {
        if items.len() > max_items as usize {
            return Err(BindingInputSchemaViolation::new(
                path,
                format!("array with maxItems {max_items}"),
                format!("array length {}", items.len()),
            ));
        }
    }
    if let Some(item_schema) = schema.get("items") {
        for (index, item) in items.iter().enumerate() {
            validate_binding_input_at_path(item_schema, item, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

fn validate_object_binding_input(
    schema: &Value,
    input: &Value,
    path: &str,
) -> Result<(), BindingInputSchemaViolation> {
    let Some(object) = input.as_object() else {
        return Err(type_violation(path, "object", input));
    };
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for field in required.iter().filter_map(Value::as_str) {
        if !object.contains_key(field) {
            let field_path = format!("{path}.{field}");
            let expected = properties
                .get(field)
                .map(describe_expected)
                .unwrap_or_else(|| String::from("required value"));
            return Err(BindingInputSchemaViolation::new(
                field_path,
                expected,
                "missing value",
            ));
        }
    }
    for (field, value) in object {
        let field_path = format!("{path}.{field}");
        if let Some(field_schema) = properties.get(field) {
            validate_binding_input_at_path(field_schema, value, &field_path)?;
            continue;
        }
        match schema.get("additionalProperties") {
            Some(Value::Bool(false)) => {
                return Err(BindingInputSchemaViolation::new(
                    field_path,
                    "no additional properties",
                    describe_value(value),
                ));
            }
            Some(additional_schema) => {
                validate_binding_input_at_path(additional_schema, value, &field_path)?;
            }
            None => {}
        }
    }
    Ok(())
}

fn has_object_keywords(schema: &Value) -> bool {
    schema.get("properties").is_some()
        || schema.get("required").is_some()
        || schema.get("additionalProperties").is_some()
}

fn type_violation(path: &str, expected: &str, input: &Value) -> BindingInputSchemaViolation {
    BindingInputSchemaViolation::new(path, expected, describe_value(input))
}

fn describe_expected(schema: &Value) -> String {
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array) {
        return format!(
            "one of {}",
            enum_values
                .iter()
                .map(compact_json)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if let Some(expected) = schema.get("const") {
        return format!("constant {}", compact_json(expected));
    }
    match schema.get("type") {
        Some(Value::String(expected_type)) => expected_type.clone(),
        Some(Value::Array(expected_types)) => expected_types
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" | "),
        _ if has_object_keywords(schema) => String::from("object"),
        _ => String::from("value"),
    }
}

fn describe_value(value: &Value) -> String {
    match value {
        Value::Null => String::from("null"),
        Value::Bool(_) => String::from("boolean"),
        Value::Number(number) => {
            let is_integer = number.as_i64().is_some()
                || number.as_u64().is_some()
                || number.as_f64().is_some_and(|float| float.fract() == 0.0);
            if is_integer {
                String::from("integer")
            } else {
                String::from("number")
            }
        }
        Value::String(_) => String::from("string"),
        Value::Array(_) => String::from("array"),
        Value::Object(_) => String::from("object"),
    }
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| String::from("<invalid json>"))
}

fn list_bindings_payload(bindings: &[Bindings]) -> Value {
    Value::Object(Map::from_iter([(
        String::from("bindings"),
        Value::Array(
            bindings
                .iter()
                .map(|collection| {
                    json_object([
                        ("name", Value::String(collection.name.clone())),
                        ("description", Value::String(collection.description.clone())),
                        (
                            "bindings",
                            Value::Array(
                                collection
                                    .bindings
                                    .iter()
                                    .map(|binding| Value::String(binding.name.clone()))
                                    .collect(),
                            ),
                        ),
                    ])
                })
                .collect(),
        ),
    )]))
}

fn describe_bindings_payload(
    bindings: &[Bindings],
    collection_name: &str,
) -> Result<Value, String> {
    let Some(collection) = bindings
        .iter()
        .find(|collection| collection.name == collection_name)
    else {
        return Err(format!(
            "No collection \"{collection_name}\". Available: {}",
            bindings_names(bindings)
        ));
    };
    Ok(json_object([
        ("name", Value::String(collection.name.clone())),
        ("description", Value::String(collection.description.clone())),
        (
            "bindings",
            Value::Object(Map::from_iter(collection.bindings.iter().map(|binding| {
                (
                    binding.name.clone(),
                    json_object([
                        ("description", Value::String(binding.description.clone())),
                        (
                            "flags",
                            Value::Array(describe_binding_flags(&binding.input_schema)),
                        ),
                    ]),
                )
            }))),
        ),
    ]))
}

fn describe_binding_payload(collection: &Bindings, binding_name: &str) -> Result<Value, String> {
    let Some(binding) = collection
        .bindings
        .iter()
        .find(|binding| binding.name == binding_name)
    else {
        return Err(format!(
            "No binding \"{binding_name}\" in collection \"{}\". Available: {}",
            collection.name,
            binding_names(collection)
        ));
    };
    Ok(json_object([
        ("collection", Value::String(collection.name.clone())),
        ("binding", Value::String(binding_name.to_string())),
        ("description", Value::String(binding.description.clone())),
        (
            "flags",
            Value::Array(describe_binding_flags(&binding.input_schema)),
        ),
        ("examples", Value::Array(Vec::new())),
    ]))
}

fn describe_binding_flags(schema: &Value) -> Vec<Value> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    properties
        .into_iter()
        .map(|(field_name, field_schema)| {
            json_object([
                (
                    "name",
                    Value::String(format!("--{}", camel_to_kebab(&field_name))),
                ),
                (
                    "type",
                    Value::String(describe_binding_flag_type(&field_schema)),
                ),
                ("required", Value::Bool(required.contains(&field_name))),
            ])
        })
        .collect()
}

fn describe_binding_flag_type(schema: &Value) -> String {
    match json_schema_type(schema) {
        Some("array") => {
            let item_type = schema
                .get("items")
                .and_then(json_schema_type)
                .unwrap_or("string");
            format!("{item_type}[]")
        }
        Some("string") => schema
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .filter(|values| !values.is_empty())
            .map(|values| values.join("|"))
            .unwrap_or_else(|| String::from("string")),
        Some(other) => other.to_string(),
        None => String::from("string"),
    }
}

fn binding_permission_mode(
    permissions: Option<&Permissions>,
    callback_key: &str,
) -> PermissionMode {
    let Some(permissions) = permissions else {
        return PermissionMode::Allow;
    };
    let Some(scope) = permissions.binding.as_ref() else {
        return PermissionMode::Allow;
    };
    match scope {
        crate::config::PatternPermissions::Mode(mode) => *mode,
        crate::config::PatternPermissions::Rules(rules) => {
            let mut mode = rules.default.unwrap_or(PermissionMode::Deny);
            for rule in &rules.rules {
                let operations_match = rule
                    .operations
                    .as_ref()
                    .map(|operations| {
                        operations
                            .iter()
                            .any(|operation| operation == "*" || operation == "invoke")
                    })
                    .unwrap_or(true);
                let patterns_match = rule
                    .patterns
                    .as_ref()
                    .map(|patterns| {
                        patterns
                            .iter()
                            .any(|pattern| permission_pattern_matches(pattern, callback_key))
                    })
                    .unwrap_or(true);
                if operations_match && patterns_match {
                    mode = rule.mode;
                }
            }
            mode
        }
    }
}

fn permission_pattern_matches(pattern: &str, value: &str) -> bool {
    if pattern == "*" || pattern == "**" || pattern == value {
        return true;
    }
    let mut pattern_index = 0;
    let mut value_index = 0;
    let pattern_bytes = pattern.as_bytes();
    let value_bytes = value.as_bytes();
    let mut star_index = None;
    let mut match_index = 0;
    while value_index < value_bytes.len() {
        if pattern_index < pattern_bytes.len()
            && pattern_bytes[pattern_index] == b'*'
            && pattern_index + 1 < pattern_bytes.len()
            && pattern_bytes[pattern_index + 1] == b'*'
        {
            star_index = Some(pattern_index);
            match_index = value_index;
            pattern_index += 2;
        } else if pattern_index < pattern_bytes.len() && pattern_bytes[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            match_index = value_index;
            pattern_index += 1;
        } else if pattern_index < pattern_bytes.len()
            && pattern_bytes[pattern_index] == value_bytes[value_index]
        {
            pattern_index += 1;
            value_index += 1;
        } else if let Some(star) = star_index {
            if pattern_bytes[star] == b'*'
                && star + 1 < pattern_bytes.len()
                && pattern_bytes[star + 1] != b'*'
                && value_bytes.get(match_index) == Some(&b':')
            {
                return false;
            }
            pattern_index = if star + 1 < pattern_bytes.len() && pattern_bytes[star + 1] == b'*' {
                star + 2
            } else {
                star + 1
            };
            match_index += 1;
            value_index = match_index;
        } else {
            return false;
        }
    }
    while pattern_index < pattern_bytes.len() && pattern_bytes[pattern_index] == b'*' {
        pattern_index += if pattern_index + 1 < pattern_bytes.len()
            && pattern_bytes[pattern_index + 1] == b'*'
        {
            2
        } else {
            1
        };
    }
    pattern_index == pattern_bytes.len()
}

fn bindings_names(bindings: &[Bindings]) -> String {
    bindings
        .iter()
        .map(|collection| collection.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn binding_names(collection: &Bindings) -> String {
    collection
        .bindings
        .iter()
        .map(|binding| binding.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_help_flag(value: &str) -> bool {
    matches!(value, "--help" | "-h")
}

fn json_schema_type(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

fn camel_to_kebab(value: &str) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() && index > 0 {
            output.push('-');
        }
        output.push(ch.to_ascii_lowercase());
    }
    output
}

fn normalize_guest_path(path: String) -> String {
    let absolute = path.starts_with('/');
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let normalized = parts.join("/");
    if absolute {
        format!("/{normalized}")
    } else {
        normalized
    }
}

fn json_object<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Object(Map::from_iter(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value)),
    ))
}

/// Build the wire [`wire::PackageDescriptor`]s for the `/opt/agentos` projection.
/// The sidecar reads package metadata from the forwarded package path.
pub(crate) fn build_package_descriptors(config: &AgentOsConfig) -> Vec<wire::PackageDescriptor> {
    config
        .packages
        .iter()
        .map(|package| wire::PackageDescriptor {
            path: package.path.clone(),
        })
        .collect()
}

fn projected_agents_from_wire(agents: Vec<wire::AgentosProjectedAgent>) -> Vec<ProjectedAgent> {
    agents
        .into_iter()
        .map(|agent| ProjectedAgent {
            id: agent.id,
            acp_entrypoint: agent.acp_entrypoint,
            adapter_entrypoint: agent.adapter_entrypoint,
        })
        .collect()
}

fn register_projected_agents(
    projected_agents: &parking_lot::Mutex<Vec<ProjectedAgent>>,
    agents: Vec<ProjectedAgent>,
) {
    let mut guard = projected_agents.lock();
    for agent in agents {
        guard.retain(|existing| existing.id != agent.id);
        guard.push(agent);
    }
}

pub(crate) fn serialize_mounts(
    config: &AgentOsConfig,
) -> Result<Vec<wire::MountDescriptor>, ClientError> {
    config
        .mounts
        .iter()
        .map(|mount| match mount {
            MountConfig::Native {
                path,
                plugin,
                guest_source,
                guest_fstype,
                read_only,
            } => {
                let plugin_config = plugin
                    .config
                    .clone()
                    .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
                Ok(wire::MountDescriptor {
                    guest_path: path.clone(),
                    guest_source: guest_source.clone().unwrap_or_else(|| plugin.id.clone()),
                    guest_fstype: guest_fstype.clone().unwrap_or_else(|| plugin.id.clone()),
                    read_only: *read_only,
                    plugin: wire::MountPluginDescriptor {
                        id: plugin.id.clone(),
                        config: json_utf8(&plugin_config, "native mount plugin config")?,
                    },
                })
            }
            MountConfig::Plain { .. } => Err(ClientError::Sidecar(
                "plain mounts cannot be configured during Rust client VM creation".to_string(),
            )),
            MountConfig::Overlay { .. } => Err(ClientError::Sidecar(
                "overlay mounts cannot be configured during Rust client VM creation".to_string(),
            )),
        })
        .collect()
}

pub(crate) fn permissions_policy(config: &AgentOsConfig) -> wire::PermissionsPolicy {
    let Some(permissions) = config.permissions.as_ref() else {
        return default_permissions_policy();
    };

    wire::PermissionsPolicy {
        fs: Some(
            permissions
                .fs
                .as_ref()
                .map(serialize_fs_permissions)
                .unwrap_or(wire::FsPermissionScope::PermissionMode(
                    wire::PermissionMode::Allow,
                )),
        ),
        network: Some(
            permissions
                .network
                .as_ref()
                .map(serialize_pattern_permissions)
                .unwrap_or_else(default_network_egress_scope),
        ),
        child_process: Some(
            permissions
                .child_process
                .as_ref()
                .map(serialize_pattern_permissions)
                .unwrap_or(wire::PatternPermissionScope::PermissionMode(
                    wire::PermissionMode::Allow,
                )),
        ),
        process: Some(
            permissions
                .process
                .as_ref()
                .map(serialize_pattern_permissions)
                .unwrap_or(wire::PatternPermissionScope::PermissionMode(
                    wire::PermissionMode::Allow,
                )),
        ),
        env: Some(
            permissions
                .env
                .as_ref()
                .map(serialize_pattern_permissions)
                .unwrap_or(wire::PatternPermissionScope::PermissionMode(
                    wire::PermissionMode::Allow,
                )),
        ),
        binding: Some(
            permissions
                .binding
                .as_ref()
                .map(serialize_pattern_permissions)
                .unwrap_or(wire::PatternPermissionScope::PermissionMode(
                    wire::PermissionMode::Allow,
                )),
        ),
    }
}

/// Default permission policy (wire form) when the client supplies no
/// `permissions`: allow-all for fs/childProcess/process/env/binding, with network
/// egress restricted to the default LLM allowlist
/// (see [`default_network_egress_scope`]).
fn default_permissions_policy() -> wire::PermissionsPolicy {
    wire::PermissionsPolicy {
        fs: Some(wire::FsPermissionScope::PermissionMode(
            wire::PermissionMode::Allow,
        )),
        network: Some(default_network_egress_scope()),
        child_process: Some(wire::PatternPermissionScope::PermissionMode(
            wire::PermissionMode::Allow,
        )),
        process: Some(wire::PatternPermissionScope::PermissionMode(
            wire::PermissionMode::Allow,
        )),
        env: Some(wire::PatternPermissionScope::PermissionMode(
            wire::PermissionMode::Allow,
        )),
        binding: Some(wire::PatternPermissionScope::PermissionMode(
            wire::PermissionMode::Allow,
        )),
    }
}

fn serialize_fs_permissions(permissions: &crate::config::FsPermissions) -> wire::FsPermissionScope {
    match permissions {
        crate::config::FsPermissions::Mode(mode) => {
            wire::FsPermissionScope::PermissionMode(serialize_permission_mode(*mode))
        }
        crate::config::FsPermissions::Rules(rules) => {
            wire::FsPermissionScope::FsPermissionRuleSet(wire::FsPermissionRuleSet {
                default: rules.default.map(serialize_permission_mode),
                rules: rules
                    .rules
                    .iter()
                    .map(|rule| wire::FsPermissionRule {
                        mode: serialize_permission_mode(rule.mode),
                        operations: operation_wildcard_if_omitted(&rule.operations),
                        paths: resource_wildcard_if_omitted(&rule.paths),
                    })
                    .collect(),
            })
        }
    }
}

fn serialize_pattern_permissions(
    permissions: &crate::config::PatternPermissions,
) -> wire::PatternPermissionScope {
    match permissions {
        crate::config::PatternPermissions::Mode(mode) => {
            wire::PatternPermissionScope::PermissionMode(serialize_permission_mode(*mode))
        }
        crate::config::PatternPermissions::Rules(rules) => {
            wire::PatternPermissionScope::PatternPermissionRuleSet(wire::PatternPermissionRuleSet {
                default: rules.default.map(serialize_permission_mode),
                rules: rules
                    .rules
                    .iter()
                    .map(|rule| wire::PatternPermissionRule {
                        mode: serialize_permission_mode(rule.mode),
                        operations: operation_wildcard_if_omitted(&rule.operations),
                        patterns: resource_wildcard_if_omitted(&rule.patterns),
                    })
                    .collect(),
            })
        }
    }
}

fn serialize_permission_mode(mode: crate::config::PermissionMode) -> wire::PermissionMode {
    match mode {
        crate::config::PermissionMode::Allow => wire::PermissionMode::Allow,
        crate::config::PermissionMode::Deny => wire::PermissionMode::Deny,
    }
}

fn json_utf8(value: &serde_json::Value, context: &str) -> Result<String, ClientError> {
    serde_json::to_string(value)
        .map_err(|error| ClientError::Sidecar(format!("failed to serialize {context}: {error}")))
}

fn operation_wildcard_if_omitted(values: &Option<Vec<String>>) -> Vec<String> {
    values.clone().unwrap_or_else(|| vec!["*".to_string()])
}

fn resource_wildcard_if_omitted(values: &Option<Vec<String>>) -> Vec<String> {
    values.clone().unwrap_or_else(|| vec!["**".to_string()])
}

/// Extract the `vm_id` from a generated ownership scope, if it is VM-scoped.
fn wire_ownership_vm_id(ownership: &wire::OwnershipScope) -> Option<&str> {
    match ownership {
        wire::OwnershipScope::VmOwnership(ownership) => Some(ownership.vm_id.as_str()),
        wire::OwnershipScope::ConnectionOwnership(_)
        | wire::OwnershipScope::SessionOwnership(_) => None,
    }
}

/// Map a `Rejected` response into a [`ClientError::Kernel`] so the errno `code` survives.
fn rejected_to_error(rejected: wire::RejectedResponse) -> ClientError {
    ClientError::Kernel {
        code: rejected.code,
        message: rejected.message,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        abort_tracked_task, default_permissions_policy, permissions_policy,
        serialize_create_vm_config_for_sidecar, serialize_root_filesystem_config_for_sidecar,
        JoinHandle,
    };
    use crate::config::{
        AgentOsConfig, AgentOsLimits, BindingLimits, FsPermissionRule, FsPermissions, HttpLimits,
        JsRuntimeLimits, MountPlugin, PatternPermissions, PermissionMode, Permissions,
        PythonLimits, ResourceLimits, RootFilesystemConfig, RootFilesystemKind, RootFilesystemMode,
        RootLowerInput, RulePermissions, WasmLimits,
    };
    use crate::fs::{
        DirEntryType, FilesystemEntry, FilesystemEntryEncoding, FilesystemSnapshotEntries,
        FilesystemSnapshotExport, RootSnapshotExport, SnapshotExportKind,
    };
    use agentos_sidecar_client::wire::{
        FsPermissionScope, PatternPermissionScope, PermissionMode as WirePermissionMode,
    };
    use agentos_vm_config::{
        RootFilesystemEntryKind, RootFilesystemLowerDescriptor,
        RootFilesystemMode as ConfigRootFilesystemMode,
    };

    /// Regression for the ACP event-pump leak (M7): `spawn_acp_event_pump` now stores its task
    /// handle in `AgentOsInner::acp_event_pump`, and `shutdown` aborts it through `abort_tracked_task`
    /// so the pump cannot outlive the disposed VM (it otherwise only ends on a shared-transport
    /// close that never comes while sibling VMs hold the transport open).
    ///
    /// Gap: driving `spawn_acp_event_pump` itself needs a live `AgentOs` (it calls
    /// `client.transport().subscribe_wire_events()`), which requires a real sidecar transport and so
    /// is out of reach at unit level. We instead exercise the exact field (`Mutex<Option<JoinHandle>>`)
    /// and the precise store-then-abort sequence the production code uses: `acp_event_pump` is
    /// initialized to `None`, `spawn_acp_event_pump` does `*slot.lock() = Some(handle)`, and
    /// `shutdown` does `abort_tracked_task(&slot)`.
    #[tokio::test]
    async fn abort_tracked_task_aborts_and_clears_the_handle() {
        // Mirrors `AgentOsInner` init (`acp_event_pump: parking_lot::Mutex::new(None)`).
        let slot: parking_lot::Mutex<Option<JoinHandle<()>>> = parking_lot::Mutex::new(None);
        assert!(
            slot.lock().is_none(),
            "pump slot starts empty like AgentOsInner"
        );

        let task = tokio::spawn(async {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            }
        });
        let abort_handle = task.abort_handle();
        // Mirrors the tail of `spawn_acp_event_pump`: `*client.inner.acp_event_pump.lock() = Some(handle)`.
        *slot.lock() = Some(task);
        assert!(
            slot.lock().is_some(),
            "spawning the pump must populate the tracked handle"
        );

        assert!(!abort_handle.is_finished(), "pump task should start alive");

        abort_tracked_task(&slot);

        assert!(
            slot.lock().is_none(),
            "tracked handle must be taken on abort"
        );

        // The abort is asynchronous; give the runtime a bounded window to reap the cancelled task.
        for _ in 0..100 {
            if abort_handle.is_finished() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            abort_handle.is_finished(),
            "pump task must be aborted on shutdown"
        );
    }

    #[test]
    fn permissions_policy_defaults_to_default_policy_when_unset() {
        assert_eq!(
            permissions_policy(&AgentOsConfig::default()),
            default_permissions_policy()
        );
    }

    #[test]
    fn default_network_egress_is_llm_allowlist_not_allow_all() {
        let policy = permissions_policy(&AgentOsConfig::default());

        // fs/childProcess/process/env stay allow-all (the VM is the boundary).
        assert_eq!(
            policy.child_process,
            Some(PatternPermissionScope::PermissionMode(
                WirePermissionMode::Allow
            ))
        );

        // Network egress is a deny-by-default allowlist of LLM provider hosts,
        // covering both DNS resolution and the TCP connection for each host.
        let Some(PatternPermissionScope::PatternPermissionRuleSet(rules)) = policy.network else {
            panic!("expected default network egress to be a rule set, not allow-all");
        };
        assert_eq!(rules.default, Some(WirePermissionMode::Deny));
        assert_eq!(rules.rules.len(), 1);
        assert_eq!(rules.rules[0].mode, WirePermissionMode::Allow);
        let patterns = &rules.rules[0].patterns;
        assert!(patterns.contains(&"dns://api.anthropic.com".to_string()));
        assert!(patterns.contains(&"tcp://api.anthropic.com:*".to_string()));
        assert!(patterns.contains(&"dns://api.openai.com".to_string()));
        assert!(patterns.contains(&"dns://generativelanguage.googleapis.com".to_string()));
        assert!(patterns.contains(&"dns://openrouter.ai".to_string()));
    }

    #[test]
    fn permissions_policy_preserves_configured_denies_and_allows_omitted_domains() {
        let policy = permissions_policy(&AgentOsConfig {
            permissions: Some(Permissions {
                network: Some(PatternPermissions::Mode(PermissionMode::Deny)),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            policy.network,
            Some(PatternPermissionScope::PermissionMode(
                WirePermissionMode::Deny
            ))
        );
        assert_eq!(
            policy.child_process,
            Some(PatternPermissionScope::PermissionMode(
                WirePermissionMode::Allow
            ))
        );
    }

    #[test]
    fn permissions_policy_expands_omitted_rule_fields_to_domain_wildcards() {
        let policy = permissions_policy(&AgentOsConfig {
            permissions: Some(Permissions {
                fs: Some(FsPermissions::Rules(RulePermissions {
                    default: Some(PermissionMode::Deny),
                    rules: vec![FsPermissionRule {
                        mode: PermissionMode::Allow,
                        operations: None,
                        paths: Some(vec!["/workspace/**".to_string()]),
                    }],
                })),
                ..Default::default()
            }),
            ..Default::default()
        });

        let Some(FsPermissionScope::FsPermissionRuleSet(rules)) = policy.fs else {
            panic!("expected fs rule set");
        };
        assert_eq!(rules.default, Some(WirePermissionMode::Deny));
        assert_eq!(rules.rules[0].operations, vec!["*"]);
        assert_eq!(rules.rules[0].paths, vec!["/workspace/**"]);

        let policy = permissions_policy(&AgentOsConfig {
            permissions: Some(Permissions {
                network: Some(PatternPermissions::Rules(RulePermissions {
                    default: Some(PermissionMode::Allow),
                    rules: vec![crate::config::PatternPermissionRule {
                        mode: PermissionMode::Deny,
                        operations: None,
                        patterns: None,
                    }],
                })),
                ..Default::default()
            }),
            ..Default::default()
        });

        let Some(PatternPermissionScope::PatternPermissionRuleSet(rules)) = policy.network else {
            panic!("expected network rule set");
        };
        assert_eq!(rules.default, Some(WirePermissionMode::Allow));
        assert_eq!(rules.rules[0].operations, vec!["*"]);
        assert_eq!(rules.rules[0].patterns, vec!["**"]);
    }

    #[test]
    fn root_filesystem_serializer_preserves_configured_descriptor() {
        let (descriptor, native_root) =
            serialize_root_filesystem_config_for_sidecar(&RootFilesystemConfig {
                mode: Some(RootFilesystemMode::ReadOnly),
                disable_default_base_layer: true,
                lowers: vec![
                    RootLowerInput::BundledBaseFilesystem,
                    RootLowerInput::SnapshotExport(RootSnapshotExport {
                        kind: SnapshotExportKind::SnapshotExport,
                        source: FilesystemSnapshotExport {
                            format: "agentos-filesystem-snapshot-v1".to_string(),
                            filesystem: FilesystemSnapshotEntries {
                                entries: vec![
                                    FilesystemEntry {
                                        path: "/bin/run".to_string(),
                                        entry_type: DirEntryType::File,
                                        mode: "0755".to_string(),
                                        uid: 1000,
                                        gid: 1000,
                                        content: Some("#!/bin/sh".to_string()),
                                        encoding: Some(FilesystemEntryEncoding::Utf8),
                                        target: None,
                                    },
                                    FilesystemEntry {
                                        path: "/link".to_string(),
                                        entry_type: DirEntryType::Symlink,
                                        mode: "0777".to_string(),
                                        uid: 0,
                                        gid: 0,
                                        content: None,
                                        encoding: None,
                                        target: Some("/bin/run".to_string()),
                                    },
                                ],
                            },
                        },
                    }),
                ],
                ..Default::default()
            })
            .expect("serialize root filesystem");

        assert!(native_root.is_none());
        assert_eq!(descriptor.mode, ConfigRootFilesystemMode::ReadOnly);
        assert!(descriptor.disable_default_base_layer);
        assert_eq!(descriptor.bootstrap_entries, Vec::new());
        assert!(matches!(
            descriptor.lowers[0],
            RootFilesystemLowerDescriptor::BundledBaseFilesystem
        ));

        let RootFilesystemLowerDescriptor::Snapshot { entries } = &descriptor.lowers[1] else {
            panic!("expected snapshot lower");
        };
        assert_eq!(entries[0].path, "/bin/run");
        assert_eq!(entries[0].kind, RootFilesystemEntryKind::File);
        assert_eq!(entries[0].mode, Some(0o755));
        assert!(entries[0].executable);
        assert_eq!(entries[1].kind, RootFilesystemEntryKind::Symlink);
        assert_eq!(entries[1].target.as_deref(), Some("/bin/run"));
    }

    #[test]
    fn create_vm_config_preserves_native_root_config() {
        let config = serialize_create_vm_config_for_sidecar(&AgentOsConfig {
            root_filesystem: RootFilesystemConfig {
                kind: RootFilesystemKind::Native,
                mode: Some(RootFilesystemMode::ReadOnly),
                native_plugin: Some(MountPlugin {
                    id: "sqlite_vfs".to_string(),
                    config: Some(serde_json::json!({
                        "databasePath": "/tmp/agentos-root.sqlite"
                    })),
                }),
                ..Default::default()
            },
            ..Default::default()
        })
        .expect("serialize create VM config");
        let native_root = config.native_root.expect("native root config");

        assert_eq!(native_root.plugin.id, "sqlite_vfs");
        assert_eq!(
            native_root.plugin.config,
            serde_json::json!({ "databasePath": "/tmp/agentos-root.sqlite" })
        );
        assert!(native_root.read_only);
    }

    #[test]
    fn create_vm_config_preserves_typed_limits() {
        let config = serialize_create_vm_config_for_sidecar(&AgentOsConfig {
            limits: Some(AgentOsLimits {
                resources: Some(ResourceLimits {
                    max_processes: Some(7),
                    max_filesystem_bytes: Some(4096),
                    ..Default::default()
                }),
                http: Some(HttpLimits {
                    max_fetch_response_bytes: Some(1024),
                }),
                bindings: Some(BindingLimits {
                    default_binding_timeout_ms: Some(500),
                    max_registered_bindings_per_vm: Some(12),
                    ..Default::default()
                }),
                js_runtime: Some(JsRuntimeLimits {
                    v8_heap_limit_mb: Some(64),
                    sync_rpc_wait_timeout_ms: Some(2_000),
                    cpu_time_limit_ms: Some(30_000),
                    wall_clock_limit_ms: Some(0),
                    import_cache_materialize_timeout_ms: Some(30_000),
                    ..Default::default()
                }),
                python: Some(PythonLimits {
                    max_old_space_mb: Some(256),
                    ..Default::default()
                }),
                wasm: Some(WasmLimits {
                    prewarm_timeout_ms: Some(30_000),
                    runner_heap_limit_mb: Some(2_048),
                    runner_cpu_time_limit_ms: Some(60_000),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        })
        .expect("serialize create VM config");
        let limits = config.limits.expect("limits config");

        let resources = limits.resources.expect("resource limits");
        assert_eq!(resources.max_processes, Some(7));
        assert_eq!(resources.max_filesystem_bytes, Some(4096));
        assert_eq!(
            limits.http.expect("http limits").max_fetch_response_bytes,
            Some(1024)
        );
        assert_eq!(
            limits
                .bindings
                .as_ref()
                .expect("binding limits")
                .default_binding_timeout_ms,
            Some(500)
        );
        assert_eq!(
            limits
                .bindings
                .expect("binding limits")
                .max_registered_bindings_per_vm,
            Some(12)
        );
        assert_eq!(
            limits
                .js_runtime
                .as_ref()
                .expect("js runtime limits")
                .v8_heap_limit_mb,
            Some(64)
        );
        let js_runtime = limits.js_runtime.expect("js runtime limits");
        assert_eq!(js_runtime.sync_rpc_wait_timeout_ms, Some(2_000));
        assert_eq!(js_runtime.cpu_time_limit_ms, Some(30_000));
        assert_eq!(js_runtime.wall_clock_limit_ms, Some(0));
        assert_eq!(js_runtime.import_cache_materialize_timeout_ms, Some(30_000));
        assert_eq!(
            limits.python.expect("python limits").max_old_space_mb,
            Some(256)
        );
        let wasm = limits.wasm.expect("wasm limits");
        assert_eq!(wasm.prewarm_timeout_ms, Some(30_000));
        assert_eq!(wasm.runner_heap_limit_mb, Some(2_048));
        assert_eq!(wasm.runner_cpu_time_limit_ms, Some(60_000));
    }
}
