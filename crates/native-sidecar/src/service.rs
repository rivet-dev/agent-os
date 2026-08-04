use crate::bindings::register_host_callbacks;
use crate::bridge::{build_mount_plugin_registry, MountPluginContext};
pub(crate) use crate::execution::{
    apply_active_process_default_signal, build_javascript_socket_path_context,
    canonical_signal_name, deferred_kernel_wait_request_for_process,
    dispatch_loopback_http_request_deferred, error_code, flush_pending_kernel_stdin,
    format_tcp_resource, ignore_stale_javascript_sync_rpc_response, javascript_sync_rpc_arg_i32,
    javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32, javascript_sync_rpc_arg_u32_optional,
    javascript_sync_rpc_arg_u64, javascript_sync_rpc_arg_u64_optional,
    javascript_sync_rpc_bytes_arg, javascript_sync_rpc_bytes_value, javascript_sync_rpc_encoding,
    javascript_sync_rpc_error_code, javascript_sync_rpc_may_make_fd_readable,
    javascript_sync_rpc_may_make_fd_writable, javascript_sync_rpc_option_bool,
    javascript_sync_rpc_option_u32, kernel_poll_response, kernel_stdin_read_response,
    mark_execute_exit_event_queued, parse_kernel_poll_args, parse_kernel_stdin_read_args,
    parse_signal, record_execute_exit_event_queue_wait, record_execute_phase,
    sanitize_javascript_child_process_internal_bootstrap_env,
    service_javascript_kernel_fd_write_sync_rpc, service_javascript_sync_rpc,
    JavascriptSyncRpcServiceRequest, LoopbackHttpDispatchRequest,
};
use crate::extension::{
    Extension, ExtensionBufferedProcessOutput, ExtensionContext, ExtensionFuture, ExtensionHost,
    ExtensionServices, ExtensionSnapshot,
};
use crate::filesystem::{
    guest_filesystem_call as filesystem_guest_filesystem_call, guest_filesystem_call_vm,
};
use crate::limits::DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT;
use crate::process_event_broker::{
    ProcessEventBroker, ProcessEventBrokerError, ProcessEventIngress, ProcessEventTarget,
};
use crate::protocol::{
    CloseStdinRequest, DisposeReason, EventFrame, EventPayload, ExecuteRequest, ExtEnvelope,
    GuestFilesystemCallRequest, GuestFilesystemResultResponse, JavascriptChildProcessSpawnOptions,
    JavascriptChildProcessSpawnRequest, KillProcessRequest, OpenSessionRequest, OwnershipScope,
    ProcessKilledResponse, ProcessStartedResponse, RejectedResponse, RequestFrame, RequestId,
    RequestPayload, ResponseFrame, ResponsePayload, SidecarRequestFrame, SidecarRequestPayload,
    SidecarResponseFrame, SidecarResponsePayload, SidecarResponseTracker,
    SidecarResponseTrackerError, SignalDispositionAction, StdinClosedResponse,
    StdinWrittenResponse, VmLifecycleState, WriteStdinRequest,
};
use crate::request_operations::OperationCancellationReason;
use crate::state::{
    ActiveExecutionEvent, BridgeError, ConnectionState, EventSinkTransport, JavascriptSocketFamily,
    JavascriptSocketPathContext, ProcessEventEnvelope, QuarantinedVmGeneration, SessionState,
    SharedBridge, SharedEventSink, SharedSidecarRequestClient, SidecarRequestTransport, VmRegistry,
    VmState, EXECUTION_DRIVER_NAME,
};
use crate::NativeSidecarBridge;
use agentos_bridge::queue_tracker::{register_queue, QueueGauge, TrackedLimit};
use agentos_bridge::{
    CommandPermissionRequest, EnvironmentAccess, EnvironmentPermissionRequest, FilesystemAccess,
    FilesystemPermissionRequest, LifecycleEventRecord, LifecycleState, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, StructuredEventRecord,
};
use agentos_execution::{
    record_sync_bridge_request_observed, JavascriptExecutionError, JavascriptSyncRpcRequest,
    PythonExecutionError, WasmExecutionError,
};
use agentos_kernel::kernel::KernelError;
use agentos_kernel::mount_plugin::{FileSystemPluginRegistry, PluginError};
use agentos_kernel::permissions::{
    CommandAccessRequest, EnvAccessRequest, EnvironmentOperation, NetworkAccessRequest,
    NetworkOperation, PermissionDecision,
};
use agentos_native_sidecar_core::permissions::{
    deny_all_policy, environment_permission_capability,
    evaluate_matching_pattern_permission_policy, evaluate_permissions_policy,
    filesystem_permission_capability, network_permission_capability,
    permission_mode_to_kernel_decision,
};
use agentos_native_sidecar_core::{
    apply_process_signal_state_update, authenticated_response as shared_authenticated_response,
    parse_process_signal_state_request, reject as shared_reject, respond as shared_respond,
    route_request_payload, session_opened_response, unsupported_host_callback_direction_dispatch,
    validate_authenticate_versions, vm_lifecycle_event as shared_vm_lifecycle_event,
    AuthenticateVersionError, RequestRoute,
};
use agentos_runtime::metrics::ResourceMetricClass;
use agentos_vm_config::{FsPermissionScope, PermissionMode, PermissionsPolicy};
// root_fs types moved to crate::vm
use agentos_kernel::vfs::VfsError;
use serde::Deserialize;
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::time;

// Constants and type aliases moved to crate::state

const INTERNAL_JAVASCRIPT_ENTRYPOINT_ENV_KEYS: &[&str] =
    &["AGENTOS_ENTRYPOINT", "AGENTOS_BOOTSTRAP_MODULE"];
const INTERNAL_WASM_ENTRYPOINT_ENV_KEYS: &[&str] =
    &["AGENTOS_WASM_MODULE_PATH", "AGENTOS_WASM_MODULE_BASE64"];
const INTERNAL_PYTHON_ENTRYPOINT_ENV_PREFIXES: &[&str] = &["AGENTOS_PYTHON_"];
// The integration fixture includes this module as a child and consumes these
// default-limit aliases; the standalone lib-test target does not.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const MAX_PROCESS_EVENT_QUEUE: usize =
    agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const MAX_PENDING_SIDECAR_RESPONSES: usize =
    agentos_runtime::DEFAULT_PROTOCOL_MAX_PENDING_RESPONSES;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const MAX_OUTBOUND_SIDECAR_REQUESTS: usize =
    agentos_runtime::DEFAULT_PROTOCOL_MAX_OUTBOUND_REQUESTS;
#[cfg(test)]
#[allow(dead_code)]
pub(crate) const MAX_COMPLETED_SIDECAR_RESPONSES: usize =
    agentos_runtime::DEFAULT_PROTOCOL_MAX_COMPLETED_RESPONSES;
pub(crate) fn process_event_queue_overflow_error(limit: usize) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_PROCESS_EVENT_LIMIT: process event queue exceeded {limit} pending events; raise runtime.protocol.maxProcessEvents"
    ))
}

fn sidecar_response_pending_overflow_error(limit: usize) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_PENDING_RESPONSE_LIMIT: sidecar response tracker exceeded {limit} pending responses; raise runtime.protocol.maxPendingResponses"
    ))
}

fn outbound_sidecar_request_queue_overflow_error(limit: usize) -> SidecarError {
    SidecarError::InvalidState(format!(
        "ERR_AGENTOS_OUTBOUND_REQUEST_LIMIT: outbound sidecar request queue exceeded {limit} pending requests; raise runtime.protocol.maxOutboundRequests"
    ))
}

fn wire_protocol_error(error: crate::wire::ProtocolCodecError) -> SidecarError {
    SidecarError::InvalidState(format!("invalid generated wire protocol frame: {error}"))
}

pub(crate) fn wire_dispatch_result(
    result: DispatchResult,
) -> Result<crate::wire::WireDispatchResult, SidecarError> {
    crate::wire::dispatch_result_from_compat(crate::wire::CompatDispatchResult {
        response: result.response,
        events: result.events,
    })
    .map_err(wire_protocol_error)
}

pub use agentos_native_sidecar_core::DispatchResult;
// NativeSidecarConfig and SidecarError moved to crate::state
pub use crate::state::{NativeSidecarConfig, SidecarError};

/// An extension request detached from the mutable sidecar coordinator.
///
/// The transport prepares this record while it has short-lived access to the
/// process registry, then executes it as an independently tracked task. Any
/// sidecar operation the extension needs is performed through the cloneable
/// service backend in its context rather than by retaining `&mut NativeSidecar`.
pub(crate) struct PreparedExtensionRequest {
    request: RequestFrame,
    namespace: String,
    payload: Vec<u8>,
    extension: Arc<dyn Extension>,
    services: Arc<dyn ExtensionServices>,
    snapshot: ExtensionSnapshot,
}

pub(crate) struct CompletedExtensionRequest {
    request: RequestFrame,
    namespace: String,
    result: Result<crate::extension::ExtensionResponse, SidecarError>,
}

impl CompletedExtensionRequest {
    pub(crate) fn failed(&self) -> bool {
        self.result.is_err()
    }
}

impl PreparedExtensionRequest {
    pub(crate) async fn execute(self) -> CompletedExtensionRequest {
        let PreparedExtensionRequest {
            request,
            namespace,
            payload,
            extension,
            services,
            snapshot,
        } = self;
        let ctx = ExtensionContext::with_services(snapshot, services);
        let result = {
            let request_future = extension.handle_request(ctx, payload);
            request_future.await
        };
        CompletedExtensionRequest {
            request,
            namespace,
            result,
        }
    }
}

type PreparedRequestFuture =
    Pin<Box<dyn Future<Output = Result<DispatchResult, SidecarError>> + 'static>>;

/// Bounded coordinator-side effects produced by one detached request.
///
/// The owned operation records effects without retaining the central sidecar.
/// Completion drains them while the coordinator is briefly available, before
/// the terminal response is made visible. `Rc<RefCell<_>>` is intentional: the
/// unified protocol loop executes these non-`Send` futures on one local task.
#[derive(Clone)]
pub(crate) struct RequestCompletionEffects {
    state: Rc<RefCell<RequestCompletionEffectState>>,
}

struct RequestCompletionEffectState {
    exited_process_ids: BTreeSet<String>,
    exited_process_limit: usize,
    exited_process_limit_path: &'static str,
    warned: bool,
}

impl Default for RequestCompletionEffects {
    fn default() -> Self {
        Self::new(1, "limits.execution.maxCompletedExecutions")
    }
}

impl RequestCompletionEffects {
    fn new(exited_process_limit: usize, exited_process_limit_path: &'static str) -> Self {
        Self {
            state: Rc::new(RefCell::new(RequestCompletionEffectState {
                exited_process_ids: BTreeSet::new(),
                exited_process_limit: exited_process_limit.max(1),
                exited_process_limit_path,
                warned: false,
            })),
        }
    }

    pub(crate) fn record_exited_process(&self, process_id: &str) -> Result<(), SidecarError> {
        let mut state = self.state.borrow_mut();
        if state.exited_process_ids.contains(process_id) {
            return Ok(());
        }
        let observed = state.exited_process_ids.len().saturating_add(1);
        if observed > state.exited_process_limit {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_REQUEST_COMPLETION_EFFECT_LIMIT: exited-process completion effects observed={observed} limit={} limit_path={}; raise {}",
                state.exited_process_limit,
                state.exited_process_limit_path,
                state.exited_process_limit_path,
            )));
        }
        state.exited_process_ids.insert(process_id.to_owned());
        let warning_threshold = state
            .exited_process_limit
            .saturating_mul(3)
            .div_ceil(4)
            .max(1);
        if !state.warned && state.exited_process_ids.len() >= warning_threshold {
            state.warned = true;
            tracing::warn!(
                limit = state.exited_process_limit,
                limit_path = state.exited_process_limit_path,
                used = state.exited_process_ids.len(),
                "request completion exited-process effects are near their bound"
            );
        }
        Ok(())
    }

    pub(crate) fn remaining_exited_process_capacity(&self, reserved: usize) -> usize {
        let state = self.state.borrow();
        state
            .exited_process_limit
            .saturating_sub(state.exited_process_ids.len())
            .saturating_sub(reserved)
    }

    fn take_exited_processes(&self) -> BTreeSet<String> {
        std::mem::take(&mut self.state.borrow_mut().exited_process_ids)
    }
}

/// A non-extension request that no longer borrows the process coordinator.
///
/// Preparation performs only the short ownership/state critical section needed
/// to clone or snapshot request inputs. The owned future may then run under the
/// request supervisor without an `Arc<Mutex<NativeSidecar<_>>>` or another
/// whole-sidecar lease.
#[derive(Clone)]
pub(crate) enum PreparedMembershipCommit {
    Connection {
        connection_id: String,
        auth_token: String,
    },
    Session {
        connection_id: String,
        session_id: String,
        placement: crate::protocol::SidecarPlacement,
        metadata: BTreeMap<String, String>,
    },
}

pub(crate) struct PreparedRequest {
    request: RequestFrame,
    operation: PreparedRequestFuture,
    effects: RequestCompletionEffects,
    committed_membership: Option<PreparedMembershipCommit>,
}

/// Terminal value returned by an independently executing [`PreparedRequest`].
/// The original request is retained so success and failure both preserve the
/// request ID and ownership scope.
pub(crate) struct CompletedRequest {
    request: RequestFrame,
    result: Result<DispatchResult, SidecarError>,
    effects: RequestCompletionEffects,
    committed_membership: Option<PreparedMembershipCommit>,
}

impl PreparedRequest {
    pub(crate) fn from_future<F>(request: RequestFrame, operation: F) -> Self
    where
        F: Future<Output = Result<DispatchResult, SidecarError>> + 'static,
    {
        Self::from_future_with_effects(request, operation, RequestCompletionEffects::default())
    }

    fn from_future_with_effects<F>(
        request: RequestFrame,
        operation: F,
        effects: RequestCompletionEffects,
    ) -> Self
    where
        F: Future<Output = Result<DispatchResult, SidecarError>> + 'static,
    {
        Self {
            request,
            operation: Box::pin(operation),
            effects,
            committed_membership: None,
        }
    }

    fn from_vm_command<F>(request: RequestFrame, operation: F) -> Self
    where
        F: Future<Output = Result<DispatchResult, SidecarError>> + 'static,
    {
        Self::from_future(request, operation)
    }

    fn failed(request: RequestFrame, error: SidecarError) -> Self {
        Self::from_future(request, async move { Err(error) })
    }

    fn from_future_with_membership<F>(
        request: RequestFrame,
        operation: F,
        committed_membership: PreparedMembershipCommit,
    ) -> Self
    where
        F: Future<Output = Result<DispatchResult, SidecarError>> + 'static,
    {
        let mut prepared = Self::from_future(request, operation);
        prepared.committed_membership = Some(committed_membership);
        prepared
    }

    pub(crate) fn committed_membership(&self) -> Option<&PreparedMembershipCommit> {
        self.committed_membership.as_ref()
    }

    pub(crate) async fn execute(self) -> CompletedRequest {
        let PreparedRequest {
            request,
            operation,
            effects,
            committed_membership,
        } = self;
        CompletedRequest {
            request,
            result: operation.await,
            effects,
            committed_membership,
        }
    }
}

impl CompletedRequest {
    pub(crate) fn failed(&self) -> bool {
        self.result.is_err()
    }
}

// SharedBridge struct and Clone impl moved to crate::state

#[derive(Debug, Default, Deserialize)]
struct LegacyJavascriptChildProcessSpawnOptions {
    // The V8 sync host binding still carries command/argv/options as three
    // strings. Flatten the canonical options object here so every newly added
    // field crosses that compatibility bridge automatically; keeping a second
    // hand-copied field list previously dropped POSIX spawn attributes and fd
    // mappings without an error.
    #[serde(flatten)]
    options: JavascriptChildProcessSpawnOptions,
    #[serde(default, rename = "maxBuffer")]
    max_buffer: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JavascriptHttpLoopbackRequest {
    process_id: String,
    server_id: u64,
    host: String,
    port: u16,
    request: String,
}

fn is_javascript_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host.eq_ignore_ascii_case("localhost")
}

pub(crate) fn parse_javascript_child_process_spawn_request(
    vm: &VmState,
    args: &[Value],
) -> Result<(JavascriptChildProcessSpawnRequest, Option<usize>), SidecarError> {
    if let Some(value) = args.first().cloned() {
        if let Ok(request) = serde_json::from_value::<JavascriptChildProcessSpawnRequest>(value) {
            return Ok((request, None));
        }
    }

    let command = javascript_sync_rpc_arg_str(args, 0, "child_process.spawn command")?.to_owned();
    let raw_args = javascript_sync_rpc_arg_str(args, 1, "child_process.spawn args")?;
    let raw_options = javascript_sync_rpc_arg_str(args, 2, "child_process.spawn options")?;

    let parsed_args = serde_json::from_str::<Vec<String>>(raw_args).map_err(|error| {
        SidecarError::InvalidState(format!("invalid child_process.spawn args payload: {error}"))
    })?;
    let parsed_options =
        parse_legacy_javascript_child_process_spawn_options(&vm.guest_env, raw_options)?;
    let max_buffer = parsed_options.max_buffer;
    let options = parsed_options.options;

    Ok((
        JavascriptChildProcessSpawnRequest {
            command,
            args: parsed_args,
            options,
        },
        max_buffer,
    ))
}

fn parse_legacy_javascript_child_process_spawn_options(
    vm_guest_env: &BTreeMap<String, String>,
    raw_options: &str,
) -> Result<LegacyJavascriptChildProcessSpawnOptions, SidecarError> {
    let mut parsed = serde_json::from_str::<LegacyJavascriptChildProcessSpawnOptions>(raw_options)
        .map_err(|error| {
            SidecarError::InvalidState(format!(
                "invalid child_process.spawn options payload: {error}"
            ))
        })?;
    let mut internal_bootstrap_env =
        sanitize_javascript_child_process_internal_bootstrap_env(vm_guest_env);
    internal_bootstrap_env.extend(sanitize_javascript_child_process_internal_bootstrap_env(
        &parsed.options.internal_bootstrap_env,
    ));
    parsed.options.internal_bootstrap_env = internal_bootstrap_env;
    Ok(parsed)
}

impl<B> SharedBridge<B> {
    fn new(bridge: B) -> Self {
        Self {
            inner: Arc::new(Mutex::new(bridge)),
            permissions: Arc::new(Mutex::new(BTreeMap::new())),
            #[cfg(test)]
            set_vm_permissions_outcomes: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

impl<B> SharedBridge<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub(crate) fn with_mut<T>(
        &self,
        operation: impl FnOnce(&mut B) -> Result<T, BridgeError<B>>,
    ) -> Result<T, SidecarError> {
        let mut bridge = self.inner.lock().map_err(|_| {
            SidecarError::Bridge(String::from("native sidecar bridge lock poisoned"))
        })?;
        operation(&mut bridge).map_err(|error| SidecarError::Bridge(format!("{error:?}")))
    }

    fn inspect<T>(&self, operation: impl FnOnce(&mut B) -> T) -> Result<T, SidecarError> {
        let mut bridge = self.inner.lock().map_err(|_| {
            SidecarError::Bridge(String::from("native sidecar bridge lock poisoned"))
        })?;
        Ok(operation(&mut bridge))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn queue_set_vm_permissions_result(
        &self,
        result: Result<(), SidecarError>,
    ) -> Result<(), SidecarError> {
        let mut outcomes = self.set_vm_permissions_outcomes.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar test set_vm_permissions outcome lock poisoned",
            ))
        })?;
        outcomes.push_back(result.err());
        Ok(())
    }

    pub(crate) fn emit_lifecycle(
        &self,
        vm_id: &str,
        state: LifecycleState,
    ) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_lifecycle(LifecycleEventRecord {
                vm_id: vm_id.to_owned(),
                state,
                detail: None,
            })
        })
    }

    pub(crate) fn emit_log(
        &self,
        vm_id: &str,
        message: impl Into<String>,
    ) -> Result<(), SidecarError> {
        self.with_mut(|bridge| {
            bridge.emit_log(LogRecord {
                vm_id: vm_id.to_owned(),
                level: LogLevel::Info,
                message: message.into(),
            })
        })
    }

    pub(crate) fn filesystem_decision(
        &self,
        vm_id: &str,
        path: &str,
        access: FilesystemAccess,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            filesystem_permission_capability(access),
            "fs",
            Some(path),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_filesystem_access(FilesystemPermissionRequest {
                vm_id: vm_id.to_owned(),
                path: path.to_owned(),
                access,
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn command_decision(
        &self,
        vm_id: &str,
        request: &CommandAccessRequest,
    ) -> PermissionDecision {
        if is_internal_runtime_command_request(request) {
            return PermissionDecision::allow();
        }
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            "child_process.spawn",
            "child_process",
            Some(&request.command),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_command_execution(CommandPermissionRequest {
                vm_id: vm_id.to_owned(),
                command: request.command.clone(),
                args: request.args.clone(),
                cwd: request.cwd.clone(),
                env: request.env.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn environment_decision(
        &self,
        vm_id: &str,
        request: &EnvAccessRequest,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            environment_permission_capability(request.op),
            "env",
            Some(&request.key),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_environment_access(EnvironmentPermissionRequest {
                vm_id: vm_id.to_owned(),
                access: match request.op {
                    EnvironmentOperation::Read => EnvironmentAccess::Read,
                    EnvironmentOperation::Write => EnvironmentAccess::Write,
                },
                key: request.key.clone(),
                value: request.value.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn network_decision(
        &self,
        vm_id: &str,
        request: &NetworkAccessRequest,
    ) -> PermissionDecision {
        if let Some(decision) = self.static_permission_decision(
            vm_id,
            network_permission_capability(request.op),
            "network",
            Some(&request.resource),
        ) {
            return decision;
        }
        match self.with_mut(|bridge| {
            bridge.check_network_access(NetworkPermissionRequest {
                vm_id: vm_id.to_owned(),
                access: match request.op {
                    NetworkOperation::Fetch => NetworkAccess::Fetch,
                    NetworkOperation::Http => NetworkAccess::Http,
                    NetworkOperation::Dns => NetworkAccess::Dns,
                    NetworkOperation::Listen => NetworkAccess::Listen,
                },
                resource: request.resource.clone(),
            })
        }) {
            Ok(decision) => map_bridge_permission(decision),
            Err(error) => PermissionDecision::deny(error.to_string()),
        }
    }

    pub(crate) fn require_network_access(
        &self,
        vm_id: &str,
        op: NetworkOperation,
        resource: impl Into<String>,
    ) -> Result<(), SidecarError> {
        let resource = resource.into();
        let decision = self.network_decision(
            vm_id,
            &NetworkAccessRequest {
                vm_id: vm_id.to_owned(),
                op,
                resource: resource.clone(),
            },
        );
        if decision.allow {
            return Ok(());
        }

        let message = match decision.reason.as_deref() {
            Some(reason) => format!("EACCES: permission denied, {resource}: {reason}"),
            None => format!("EACCES: permission denied, {resource}"),
        };
        Err(SidecarError::Execution(message))
    }

    /// Revalidate an authority-expanding network operation against both the
    /// requested name and the complete DNS answer immediately before use.
    ///
    /// The requested resource uses normal policy semantics. For a stored rule
    /// set, resolved addresses add restrictions only when an address rule
    /// explicitly matches; this preserves hostname allowlists and literal-IP
    /// policies. Dynamic bridge policies are asked about every resource.
    pub(crate) fn require_resolved_network_access(
        &self,
        vm_id: &str,
        op: NetworkOperation,
        requested_resource: &str,
        resolved_resources: &[String],
    ) -> Result<(), SidecarError> {
        let capability = network_permission_capability(op);
        let permissions = self
            .permissions
            .lock()
            .map_err(|_| {
                SidecarError::Bridge(String::from(
                    "native sidecar permission policy lock poisoned",
                ))
            })?
            .get(vm_id)
            .cloned();

        let require = |resource: &str, decision: PermissionDecision| {
            if decision.allow {
                return Ok(());
            }
            let message = match decision.reason.as_deref() {
                Some(reason) => format!("EACCES: permission denied, {resource}: {reason}"),
                None => format!("EACCES: permission denied, {resource}"),
            };
            Err(SidecarError::Execution(message))
        };

        if let Some(permissions) = permissions {
            let requested_mode = evaluate_permissions_policy(
                &permissions,
                "network",
                capability,
                Some(requested_resource),
            );
            require(
                requested_resource,
                permission_mode_to_kernel_decision(requested_mode, capability),
            )?;

            let mut checked = BTreeSet::new();
            for resource in resolved_resources {
                if resource == requested_resource || !checked.insert(resource.as_str()) {
                    continue;
                }
                let Some(mode) = evaluate_matching_pattern_permission_policy(
                    &permissions,
                    "network",
                    capability,
                    Some(resource),
                ) else {
                    continue;
                };
                require(
                    resource,
                    permission_mode_to_kernel_decision(mode, capability),
                )?;
            }
            return Ok(());
        }

        self.require_network_access(vm_id, op, requested_resource.to_owned())?;
        let mut checked = BTreeSet::new();
        for resource in resolved_resources {
            if resource != requested_resource && checked.insert(resource.as_str()) {
                self.require_network_access(vm_id, op, resource.clone())?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_vm_permissions(
        &self,
        vm_id: &str,
        permissions: &PermissionsPolicy,
    ) -> Result<(), SidecarError> {
        #[cfg(test)]
        {
            let mut outcomes = self.set_vm_permissions_outcomes.lock().map_err(|_| {
                SidecarError::Bridge(String::from(
                    "native sidecar test set_vm_permissions outcome lock poisoned",
                ))
            })?;
            if let Some(Some(error)) = outcomes.pop_front() {
                return Err(error);
            }
        }

        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.insert(vm_id.to_owned(), permissions.clone());
        Ok(())
    }

    pub(crate) fn restore_vm_permissions_fail_closed(
        &self,
        vm_id: &str,
        original_permissions: &PermissionsPolicy,
        context: &str,
        operation_error: &SidecarError,
    ) -> Result<(), SidecarError> {
        match self.set_vm_permissions(vm_id, original_permissions) {
            Ok(()) => Ok(()),
            Err(restore_error) => {
                let deny_all = deny_all_policy();
                match self.set_vm_permissions(vm_id, &deny_all) {
                    Ok(()) => Err(SidecarError::InvalidState(format!(
                        "{context} failed: {operation_error}; restoring original permissions failed: {restore_error}; applied deny-all fallback"
                    ))),
                    Err(deny_all_error) => panic!(
                        "{context} failed: {operation_error}; restoring original permissions failed: {restore_error}; deny-all fallback failed: {deny_all_error}"
                    ),
                }
            }
        }
    }

    pub(crate) fn clear_vm_permissions(&self, vm_id: &str) -> Result<(), SidecarError> {
        let mut stored = self.permissions.lock().map_err(|_| {
            SidecarError::Bridge(String::from(
                "native sidecar permission policy lock poisoned",
            ))
        })?;
        stored.remove(vm_id);
        Ok(())
    }

    pub(crate) fn filesystem_unrestricted(&self, vm_id: &str) -> bool {
        let Ok(stored) = self.permissions.lock() else {
            return false;
        };
        matches!(
            stored.get(vm_id).and_then(|policy| policy.fs.as_ref()),
            Some(FsPermissionScope::Mode(PermissionMode::Allow))
        )
    }

    pub(crate) fn static_permission_decision(
        &self,
        vm_id: &str,
        capability: &str,
        domain: &str,
        resource: Option<&str>,
    ) -> Option<PermissionDecision> {
        let stored = self.permissions.lock().ok()?;
        let permissions = stored.get(vm_id)?;
        let mode = evaluate_permissions_policy(permissions, domain, capability, resource);
        Some(permission_mode_to_kernel_decision(mode, capability))
    }
}

pub(crate) fn validate_permissions_policy(
    permissions: &PermissionsPolicy,
) -> Result<(), SidecarError> {
    agentos_native_sidecar_core::permissions::validate_permissions_policy(permissions)
        .map_err(|error| SidecarError::InvalidState(error.to_string()))
}

fn is_internal_runtime_command_request(request: &CommandAccessRequest) -> bool {
    match request.command.as_str() {
        "node" => request
            .env
            .keys()
            .any(|key| INTERNAL_JAVASCRIPT_ENTRYPOINT_ENV_KEYS.contains(&key.as_str())),
        "wasm" => request
            .env
            .keys()
            .any(|key| INTERNAL_WASM_ENTRYPOINT_ENV_KEYS.contains(&key.as_str())),
        "python" => request.env.keys().any(|key| {
            INTERNAL_PYTHON_ENTRYPOINT_ENV_PREFIXES
                .iter()
                .any(|prefix| key.starts_with(prefix))
        }),
        _ => false,
    }
}

fn ownership_matches_process_event(
    ownership: &OwnershipScope,
    event: &ProcessEventEnvelope,
) -> bool {
    match ownership {
        OwnershipScope::ConnectionOwnership(inner) => inner.connection_id == event.connection_id,
        OwnershipScope::SessionOwnership(inner) => {
            inner.connection_id == event.connection_id && inner.session_id == event.session_id
        }
        OwnershipScope::VmOwnership(inner) => {
            inner.connection_id == event.connection_id
                && inner.session_id == event.session_id
                && inner.vm_id == event.vm_id
        }
    }
}

fn public_process_event_matches_ownership<B>(
    sidecar: &NativeSidecar<B>,
    ownership: &OwnershipScope,
    event: &ProcessEventEnvelope,
) -> bool
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if !ownership_matches_process_event(ownership, event) {
        return false;
    }

    if event.process_id.contains('/') {
        return false;
    }

    let target = match ProcessEventTarget::new(
        event.connection_id.clone(),
        event.session_id.clone(),
        event.vm_id.clone(),
        event.process_id.clone(),
    ) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("ERR_AGENTOS_PROCESS_EVENT_TARGET: {error}");
            return false;
        }
    };
    match sidecar.process_event_broker.target_is_claimed(&target) {
        Ok(true) => return false,
        Ok(false) => {}
        Err(error) => {
            eprintln!("ERR_AGENTOS_PROCESS_EVENT_CLAIM_CHECK: {error}");
            return false;
        }
    }

    // Stale unclaimed events must still be drained through
    // handle_process_event_envelope() so the sidecar can emit the expected
    // fail-closed log when teardown wins the race.
    true
}

fn poll_future_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> Option<F::Output> {
    let mut context = Context::from_waker(Waker::noop());
    match future.poll(&mut context) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

// ConnectionState, SessionState, VmConfiguration, VmState moved to crate::state

// JavascriptSocketPathContext, JavascriptSocketFamily, VmListenPolicy moved to crate::state

impl JavascriptSocketPathContext {
    pub(crate) fn loopback_port_allowed(&self, port: u16) -> bool {
        self.loopback_exempt_ports.contains(&port)
            || self
                .tcp_loopback_guest_to_host_ports
                .keys()
                .any(|(_, guest_port)| *guest_port == port)
            || self
                .udp_loopback_guest_to_host_ports
                .keys()
                .any(|(_, guest_port)| *guest_port == port)
    }

    pub(crate) fn translate_tcp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.tcp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn http_loopback_target(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<&crate::state::JavascriptHttpLoopbackTarget> {
        self.http_loopback_targets.get(&(family, port))
    }

    pub(crate) fn translate_udp_loopback_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn guest_udp_port_for_host_port(
        &self,
        family: JavascriptSocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_host_to_guest_ports
            .get(&(family, port))
            .copied()
    }
}

// ActiveProcess, NetworkResourceCounts moved to crate::state

pub struct NativeSidecar<B> {
    pub(crate) config: NativeSidecarConfig,
    pub(crate) runtime_context: Option<agentos_runtime::RuntimeContext>,
    pub(crate) dns_resolver: agentos_kernel::dns::SharedDnsResolver,
    pub(crate) bridge: SharedBridge<B>,
    pub(crate) mount_plugins: FileSystemPluginRegistry<MountPluginContext<B>>,
    pub(crate) cache_root: PathBuf,
    pub(crate) next_connection_id: usize,
    pub(crate) next_session_id: usize,
    pub(crate) next_vm_id: usize,
    pub(crate) next_sidecar_request_id: RequestId,
    pub(crate) connections: BTreeMap<String, ConnectionState>,
    pub(crate) sessions: BTreeMap<String, SessionState>,
    pub(crate) vms: VmRegistry,
    /// Detached generations whose asynchronous ownership has not reconciled.
    /// The combined active + quarantined generation count is admitted against
    /// `runtime.resources.maxCapabilities`, keeping this collection bounded.
    pub(crate) quarantined_vms: BTreeMap<u64, QuarantinedVmGeneration>,
    #[allow(dead_code)]
    pub(crate) process_event_sender: Sender<ProcessEventEnvelope>,
    pub(crate) process_event_receiver: Option<Receiver<ProcessEventEnvelope>>,
    pub(crate) process_event_notify: Arc<tokio::sync::Notify>,
    /// Ownership-aware handoff for process-targeted consumers. The legacy raw
    /// receiver remains the compatibility source while callers migrate; a
    /// short coordinator command transfers one matching envelope into this
    /// broker before the caller performs its long wait independently.
    pub(crate) process_event_broker: ProcessEventBroker,
    pub(crate) process_event_ingress: ProcessEventIngress,
    /// The single process-level deadline task that wakes cooperative kernel
    /// zombie reaping. It is replaced only when a genuinely earlier deadline
    /// appears; never one task or OS thread per process.
    pub(crate) kernel_reaper_task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) kernel_reaper_deadline: Option<Instant>,
    /// One receiver envelope whose temporary public-queue admission failed.
    /// It is retried before reading the channel, preserving exact FIFO order.
    pub(crate) deferred_process_event_envelope: Option<ProcessEventEnvelope>,
    pub(crate) pending_process_events: VecDeque<ProcessEventEnvelope>,
    pub(crate) pending_sidecar_responses: SidecarResponseTracker,
    pub(crate) outbound_sidecar_requests: VecDeque<SidecarRequestFrame>,
    pub(crate) completed_sidecar_responses: BTreeMap<RequestId, SidecarResponseFrame>,
    pub(crate) completed_sidecar_response_order: VecDeque<RequestId>,
    pub(crate) completed_sidecar_responses_gauge: Arc<QueueGauge>,
    pub(crate) pending_process_events_gauge: Arc<QueueGauge>,
    pub(crate) pending_process_event_bytes_gauge: Arc<QueueGauge>,
    pub(crate) pending_sidecar_responses_gauge: Arc<QueueGauge>,
    pub(crate) outbound_sidecar_requests_gauge: Arc<QueueGauge>,
    pub(crate) sidecar_requests: SharedSidecarRequestClient,
    pub(crate) event_sink: SharedEventSink,
    pub(crate) extensions: BTreeMap<String, Arc<dyn Extension>>,
    /// Cloneable transport-agnostic services used by direct extension
    /// dispatch. Extension futures never receive or retain `&mut NativeSidecar`.
    pub(crate) extension_services: Option<Arc<dyn ExtensionServices>>,
    pub(crate) extension_sessions: BTreeMap<(String, String), ExtensionSessionResources>,
    pub(crate) extension_process_output_buffers:
        BTreeMap<(String, String), ExtensionBufferedProcessOutput>,
    #[cfg(test)]
    pub(crate) fail_next_exec_start_after_commit: bool,
    /// Session scopes (connection_id, session_id) disposed since the stdio
    /// transport last drained them. Lets the transport remove dead sessions from
    /// its active-session set instead of iterating them forever (M5).
    pub(crate) disposed_sessions: Vec<(String, String)>,
}

#[derive(Debug)]
pub(crate) struct ExtensionSessionResources {
    pub(crate) ownership: OwnershipScope,
    pub(crate) process_ids: BTreeSet<String>,
    pub(crate) vm_ids: BTreeSet<String>,
}

struct GuestLimitDiagnostic {
    scope: &'static str,
    current_usage: Option<u64>,
    message: String,
}

fn guest_limit_diagnostic(limit: &agentos_runtime::accounting::LimitError) -> GuestLimitDiagnostic {
    if limit.scope.starts_with("vm=") {
        return GuestLimitDiagnostic {
            scope: "vm",
            current_usage: Some(u64::try_from(limit.used).unwrap_or(u64::MAX)),
            message: limit.to_string(),
        };
    }

    GuestLimitDiagnostic {
        scope: "process",
        current_usage: None,
        message: crate::state::guest_limit_message(limit),
    }
}

impl<B> fmt::Debug for NativeSidecar<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeSidecar")
            .field("config", &self.config)
            .field("cache_root", &self.cache_root)
            .field("next_connection_id", &self.next_connection_id)
            .field("next_session_id", &self.next_session_id)
            .field("next_vm_id", &self.next_vm_id)
            .field("connection_count", &self.connections.len())
            .field("session_count", &self.sessions.len())
            .field("vm_count", &self.vms.len())
            .field("quarantined_vm_count", &self.quarantined_vms.len())
            .field("extension_session_count", &self.extension_sessions.len())
            .field(
                "extension_process_output_buffer_count",
                &self.extension_process_output_buffers.len(),
            )
            .finish()
    }
}

impl<B> NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    pub fn new(bridge: B) -> Result<Self, SidecarError> {
        Self::with_config(bridge, NativeSidecarConfig::default())
    }

    pub fn with_config(bridge: B, config: NativeSidecarConfig) -> Result<Self, SidecarError> {
        let runtime_context = agentos_runtime::SidecarRuntime::process(&config.runtime)
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?
            .context();
        Self::with_runtime_context(bridge, config, runtime_context)
    }

    fn with_runtime_context(
        bridge: B,
        config: NativeSidecarConfig,
        runtime_context: agentos_runtime::RuntimeContext,
    ) -> Result<Self, SidecarError> {
        if matches!(config.expected_auth_token.as_deref(), Some("")) {
            return Err(SidecarError::InvalidState(String::from(
                "native sidecar expected_auth_token must not be empty",
            )));
        }
        if config.acp_termination_grace.is_zero() {
            return Err(SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_RUNTIME_CONFIG: NativeSidecarConfig.acp_termination_grace must be greater than zero",
            )));
        }
        let dns_resolver: agentos_kernel::dns::SharedDnsResolver = Arc::new(
            agentos_kernel::dns::HickoryDnsResolver::with_runtime(runtime_context.clone()),
        );

        let cache_root = config.compile_cache_root.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "{}-{}",
                config.sidecar_id,
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time before unix epoch")
                    .as_nanos()
            ))
        });
        fs::create_dir_all(&cache_root).map_err(|error| {
            SidecarError::Io(format!("failed to prepare sidecar cache root: {error}"))
        })?;

        let bridge = SharedBridge::new(bridge);
        let mount_plugins = build_mount_plugin_registry::<B>()?;
        let protocol_limits = config.runtime.protocol.clone();
        let (process_event_sender, process_event_receiver) =
            channel(protocol_limits.max_process_events);
        let process_event_notify = Arc::new(tokio::sync::Notify::new());
        let (process_event_broker, process_event_ingress, process_event_broker_driver) =
            ProcessEventBroker::new(&config.runtime).map_err(|error| {
                SidecarError::InvalidState(format!(
                    "failed to initialize process event broker: {error}"
                ))
            })?;
        runtime_context
            .spawn(agentos_runtime::TaskClass::Runtime, async move {
                if let Err(error) = process_event_broker_driver.run().await {
                    eprintln!("ERR_AGENTOS_PROCESS_EVENT_BROKER_DRIVER: {error}");
                }
            })
            .map_err(SidecarError::from)?;
        Ok(Self {
            config,
            runtime_context: Some(runtime_context),
            dns_resolver,
            bridge,
            mount_plugins,
            cache_root,
            next_connection_id: 0,
            next_session_id: 0,
            next_vm_id: 0,
            next_sidecar_request_id: -1,
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            vms: VmRegistry::default(),
            quarantined_vms: BTreeMap::new(),
            process_event_sender,
            process_event_receiver: Some(process_event_receiver),
            process_event_notify,
            process_event_broker,
            process_event_ingress,
            kernel_reaper_task: None,
            kernel_reaper_deadline: None,
            deferred_process_event_envelope: None,
            pending_process_events: VecDeque::new(),
            pending_sidecar_responses: SidecarResponseTracker::default(),
            outbound_sidecar_requests: VecDeque::new(),
            completed_sidecar_responses: BTreeMap::new(),
            completed_sidecar_response_order: VecDeque::new(),
            completed_sidecar_responses_gauge: register_queue(
                TrackedLimit::CompletedSidecarResponses,
                protocol_limits.max_completed_responses,
            ),
            pending_process_events_gauge: register_queue(
                TrackedLimit::PendingProcessEvents,
                protocol_limits.max_process_events,
            ),
            pending_process_event_bytes_gauge: register_queue(
                TrackedLimit::PendingProcessEventBytes,
                agentos_native_sidecar_core::limits::DEFAULT_PROCESS_PENDING_EVENT_BYTES,
            ),
            pending_sidecar_responses_gauge: register_queue(
                TrackedLimit::PendingSidecarResponses,
                protocol_limits.max_pending_responses,
            ),
            outbound_sidecar_requests_gauge: register_queue(
                TrackedLimit::OutboundSidecarRequests,
                protocol_limits.max_outbound_requests,
            ),
            sidecar_requests: SharedSidecarRequestClient::default(),
            event_sink: SharedEventSink::default(),
            extensions: BTreeMap::new(),
            extension_services: None,
            extension_sessions: BTreeMap::new(),
            extension_process_output_buffers: BTreeMap::new(),
            #[cfg(test)]
            fail_next_exec_start_after_commit: false,
            disposed_sessions: Vec::new(),
        })
    }

    pub fn with_config_and_extensions(
        bridge: B,
        config: NativeSidecarConfig,
        extensions: Vec<Box<dyn Extension>>,
    ) -> Result<Self, SidecarError> {
        let mut sidecar = Self::with_config(bridge, config)?;
        for extension in extensions {
            sidecar.register_extension(extension)?;
        }
        Ok(sidecar)
    }

    pub fn with_config_extensions_and_runtime(
        bridge: B,
        config: NativeSidecarConfig,
        extensions: Vec<Box<dyn Extension>>,
        runtime_context: agentos_runtime::RuntimeContext,
    ) -> Result<Self, SidecarError> {
        let mut sidecar = Self::with_runtime_context(bridge, config, runtime_context)?;
        for extension in extensions {
            sidecar.register_extension(extension)?;
        }
        Ok(sidecar)
    }

    pub(crate) fn prune_extension_process_resource(&mut self, process_id: &str) {
        self.extension_sessions.retain(|_, resources| {
            resources.process_ids.remove(process_id);
            !resources.process_ids.is_empty() || !resources.vm_ids.is_empty()
        });
    }

    pub(crate) fn prune_extension_vm_resource(&mut self, vm_id: &str) {
        self.extension_sessions.retain(|_, resources| {
            if matches!(
                &resources.ownership,
                OwnershipScope::VmOwnership(inner) if inner.vm_id == vm_id
            ) {
                resources.process_ids.clear();
            }
            resources.vm_ids.remove(vm_id);
            !resources.process_ids.is_empty() || !resources.vm_ids.is_empty()
        });
    }

    /// Reclaim every per-VM tracking entry owned by the sidecar for `vm_id`.
    ///
    /// Called unconditionally from `dispose_vm_internal` so that a fallible
    /// teardown step (root-filesystem snapshot/flush, kernel dispose, permission
    /// reset) erroring out with `?` can never strand these maps for the rest of
    /// the process lifetime (H1). This also reclaims the ACP output-buffer map,
    /// which was previously removed only on a successful handoff and leaked on VM
    /// or session disposal (M6).
    pub(crate) fn reclaim_vm_tracking(&mut self, session_id: &str, vm_id: &str) {
        self.prune_extension_vm_resource(vm_id);
        self.extension_process_output_buffers
            .retain(|(buffer_vm_id, _process_id), _| buffer_vm_id != vm_id);
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.vm_ids.remove(vm_id);
        }
    }

    /// Probe buffered extension process output without retaining the sidecar
    /// coordinator across the caller's wait. When `finalize_if_empty` is false,
    /// `None` means the caller should wait on `process_event_notify` and probe
    /// again. The final probe binds the process resource and transfers ownership
    /// of the buffer even when no output arrived before the deadline.
    pub(crate) async fn probe_extension_process_output_handoff(
        &mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        finalize_if_empty: bool,
    ) -> Result<Option<ExtensionBufferedProcessOutput>, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let key = (vm_id.clone(), process_id.clone());
        self.pump_process_events(&ownership).await?;
        while let Some(envelope) = self.take_matching_process_event_envelope(&vm_id, &process_id)? {
            if self.capture_extension_process_output_event(&vm_id, &process_id, &envelope.event) {
                continue;
            }
            self.queue_pending_process_event(envelope)?;
            break;
        }
        let buffered = self
            .extension_process_output_buffers
            .get(&key)
            .is_some_and(|buffer| !buffer.stdout.is_empty() || !buffer.stderr.is_empty());
        if !buffered && !finalize_if_empty {
            return Ok(None);
        }
        self.bind_extension_process_resource(ownership, namespace, ext_session_id, process_id)?;
        self.extension_process_output_buffers
            .remove(&key)
            .map(Some)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "extension process output buffering was not started",
                ))
            })
    }

    pub(crate) fn probe_extension_process_output_handoff_nowait(
        &mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        finalize_if_empty: bool,
    ) -> Result<Option<ExtensionBufferedProcessOutput>, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let key = (vm_id.clone(), process_id.clone());
        while let Some(envelope) = self.take_matching_process_event_envelope(&vm_id, &process_id)? {
            if self.capture_extension_process_output_event(&vm_id, &process_id, &envelope.event) {
                continue;
            }
            self.queue_pending_process_event(envelope)?;
            break;
        }
        let buffered = self
            .extension_process_output_buffers
            .get(&key)
            .is_some_and(|buffer| !buffer.stdout.is_empty() || !buffer.stderr.is_empty());
        if !buffered && !finalize_if_empty {
            return Ok(None);
        }
        self.bind_extension_process_resource(ownership, namespace, ext_session_id, process_id)?;
        self.extension_process_output_buffers
            .remove(&key)
            .map(Some)
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "extension process output buffering was not started",
                ))
            })
    }

    pub(crate) fn reap_reconciled_quarantined_vms(&mut self) {
        let before = self.quarantined_vms.len();
        let mut reaped = Vec::new();
        self.quarantined_vms.retain(|generation, quarantined| {
            if quarantined.can_reap() {
                reaped.push((*generation, quarantined.vm_id.clone()));
                false
            } else {
                true
            }
        });
        for (generation, vm_id) in reaped {
            eprintln!("INFO_AGENTOS_VM_QUARANTINE_REAPED: vm_id={vm_id} generation={generation}");
        }
        if self.quarantined_vms.len() != before {
            self.observe_active_vm_generations();
        }
    }

    pub(crate) fn observe_active_vm_generations(&self) {
        if let Some(runtime_context) = self.runtime_context.as_ref() {
            runtime_context.metrics().observe_resource(
                ResourceMetricClass::ActiveVms,
                self.vms.len().saturating_add(self.quarantined_vms.len()),
            );
        }
    }

    pub(crate) fn ensure_vm_generation_capacity(&self) -> Result<(), SidecarError> {
        let limit = self.config.runtime.resources.max_capabilities;
        let used = self.vms.len().saturating_add(self.quarantined_vms.len());
        if used >= limit {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_GENERATION_LIMIT: tracked={used} limit={limit}; raise runtime.resources.maxCapabilities"
            )));
        }
        Ok(())
    }

    pub(crate) fn retain_quarantined_vm(
        &mut self,
        quarantined: QuarantinedVmGeneration,
    ) -> Result<(), SidecarError> {
        let generation = quarantined.generation;
        if self.quarantined_vms.contains_key(&generation) {
            return Err(SidecarError::Conflict(format!(
                "ERR_AGENTOS_VM_GENERATION_DUPLICATE: generation={generation} is already quarantined"
            )));
        }
        let limit = self.config.runtime.resources.max_capabilities;
        if self.quarantined_vms.len() >= limit {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_QUARANTINE_LIMIT: quarantined={} limit={limit}; raise runtime.resources.maxCapabilities",
                self.quarantined_vms.len()
            )));
        }
        self.quarantined_vms.insert(generation, quarantined);
        self.observe_active_vm_generations();
        Ok(())
    }

    pub(crate) fn capture_extension_process_output_event(
        &mut self,
        vm_id: &str,
        process_id: &str,
        event: &ActiveExecutionEvent,
    ) -> bool {
        let Some(buffer) = self
            .extension_process_output_buffers
            .get_mut(&(vm_id.to_string(), process_id.to_string()))
        else {
            return false;
        };
        match event {
            ActiveExecutionEvent::Stdout(chunk) => {
                buffer.append_stdout(chunk, DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT);
                true
            }
            ActiveExecutionEvent::Stderr(chunk) => {
                buffer.append_stderr(chunk, DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT);
                true
            }
            ActiveExecutionEvent::JavascriptSyncRpcRequest(_)
            | ActiveExecutionEvent::JavascriptSyncRpcCompletion(_)
            | ActiveExecutionEvent::PythonVfsRpcRequest(_)
            | ActiveExecutionEvent::PythonSocketConnectCompletion(_)
            | ActiveExecutionEvent::SignalState { .. }
            | ActiveExecutionEvent::Exited(_) => false,
        }
    }

    pub(crate) fn bind_extension_process_resource(
        &mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
    ) -> Result<(), SidecarError> {
        if ext_session_id.is_empty() {
            return Err(SidecarError::InvalidState(String::from(
                "extension session id must not be empty",
            )));
        }
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let process_exists = self
            .vms
            .get(&vm_id)
            .is_some_and(|vm| vm.active_processes.contains_key(&process_id));
        if !process_exists {
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} has no active process {process_id}"
            )));
        }

        let key = (namespace, ext_session_id);
        if let Some(resources) = self.extension_sessions.get_mut(&key) {
            if resources.ownership != ownership {
                return Err(SidecarError::InvalidState(String::from(
                    "extension session ownership did not match existing resources",
                )));
            }
            resources.process_ids.insert(process_id);
        } else {
            self.extension_sessions.insert(
                key,
                ExtensionSessionResources {
                    ownership,
                    process_ids: BTreeSet::from([process_id]),
                    vm_ids: BTreeSet::new(),
                },
            );
        }
        Ok(())
    }

    pub(crate) fn bind_extension_vm_resource(
        &mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> Result<(), SidecarError> {
        if ext_session_id.is_empty() {
            return Err(SidecarError::InvalidState(String::from(
                "extension session id must not be empty",
            )));
        }
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;

        let key = (namespace, ext_session_id);
        if let Some(resources) = self.extension_sessions.get_mut(&key) {
            if resources.ownership != ownership {
                return Err(SidecarError::InvalidState(String::from(
                    "extension session ownership did not match existing resources",
                )));
            }
            resources.vm_ids.insert(vm_id);
        } else {
            self.extension_sessions.insert(
                key,
                ExtensionSessionResources {
                    ownership,
                    process_ids: BTreeSet::new(),
                    vm_ids: BTreeSet::from([vm_id]),
                },
            );
        }
        Ok(())
    }

    pub(crate) fn start_buffering_process_output_nowait(
        &mut self,
        ownership: OwnershipScope,
        process_id: String,
    ) -> Result<(), SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let key = (vm_id, process_id);
        if self.extension_process_output_buffers.contains_key(&key) {
            return Err(SidecarError::Conflict(String::from(
                "extension process output buffering already started",
            )));
        }
        self.extension_process_output_buffers
            .insert(key, ExtensionBufferedProcessOutput::default());
        Ok(())
    }

    pub(crate) fn detach_extension_session_resources_for_owned_disposal(
        &mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> Result<Vec<String>, SidecarError> {
        let key = (namespace, ext_session_id);
        let Some(resources) = self.extension_sessions.get(&key) else {
            return Ok(Vec::new());
        };
        if resources.ownership != ownership {
            return Err(SidecarError::InvalidState(String::from(
                "extension session ownership did not match dispose request",
            )));
        }
        let resources = self
            .extension_sessions
            .remove(&key)
            .expect("extension resources existed before removal");
        let (_, _, vm_id) = self.vm_scope_for(&ownership)?;
        for process_id in resources.process_ids {
            if self
                .vms
                .get(&vm_id)
                .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
            {
                self.kill_process_internal(&vm_id, &process_id, "SIGTERM")?;
            }
        }
        Ok(resources.vm_ids.into_iter().collect())
    }

    pub fn sidecar_id(&self) -> &str {
        &self.config.sidecar_id
    }

    pub fn with_bridge_mut<T>(
        &self,
        operation: impl FnOnce(&mut B) -> T,
    ) -> Result<T, SidecarError> {
        self.bridge.inspect(operation)
    }

    pub fn set_sidecar_request_transport(&mut self, transport: Arc<dyn SidecarRequestTransport>) {
        self.sidecar_requests.set_transport(transport);
    }

    pub fn set_event_transport(&mut self, transport: Arc<dyn EventSinkTransport>) {
        self.event_sink.set_transport(transport);
    }

    pub(crate) fn set_extension_services(&mut self, services: Arc<dyn ExtensionServices>) {
        self.extension_services = Some(services);
    }

    pub fn register_extension(
        &mut self,
        extension: Box<dyn Extension>,
    ) -> Result<(), SidecarError> {
        let namespace = extension.namespace().to_owned();
        if namespace.is_empty() {
            return Err(SidecarError::InvalidState(String::from(
                "extension namespace must not be empty",
            )));
        }
        if self.extensions.contains_key(&namespace) {
            return Err(SidecarError::Conflict(format!(
                "extension namespace {namespace} is already registered",
            )));
        }
        self.extensions.insert(namespace, Arc::from(extension));
        Ok(())
    }

    pub fn set_sidecar_request_handler<F>(&mut self, handler: F)
    where
        F: Fn(SidecarRequestFrame) -> Result<SidecarResponsePayload, SidecarError>
            + Send
            + Sync
            + 'static,
    {
        struct HandlerTransport<F>(F);

        impl<F> SidecarRequestTransport for HandlerTransport<F>
        where
            F: Fn(SidecarRequestFrame) -> Result<SidecarResponsePayload, SidecarError>
                + Send
                + Sync
                + 'static,
        {
            fn send_request(
                &self,
                request: SidecarRequestFrame,
                _timeout: Duration,
            ) -> Result<SidecarResponseFrame, SidecarError> {
                let payload = (self.0)(request.clone())?;
                Ok(SidecarResponseFrame::new(
                    request.request_id,
                    request.ownership,
                    payload,
                ))
            }
        }

        self.set_sidecar_request_transport(Arc::new(HandlerTransport(handler)));
    }

    pub fn set_wire_sidecar_request_handler<F>(&mut self, handler: F)
    where
        F: Fn(
                crate::wire::SidecarRequestFrame,
            ) -> Result<crate::wire::SidecarResponseFrame, SidecarError>
            + Send
            + Sync
            + 'static,
    {
        self.set_sidecar_request_handler(move |request| {
            let request = crate::wire::sidecar_request_frame_from_compat(request)
                .map_err(wire_protocol_error)?;
            let response = handler(request)?;
            let response = crate::wire::sidecar_response_frame_to_compat(response)
                .map_err(wire_protocol_error)?;
            Ok(response.payload)
        });
    }

    pub(crate) fn queue_pending_process_event(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), SidecarError> {
        self.try_queue_pending_process_event(envelope)
            .map_err(|(error, _envelope)| error)
    }

    // Preserve the rejected envelope so callers can requeue it without losing
    // its retained-byte reservation or delivery ordering.
    #[allow(clippy::result_large_err)]
    pub(crate) fn try_queue_pending_process_event(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), (SidecarError, ProcessEventEnvelope)> {
        if let Err(error) = self.check_pending_process_event_capacity(&envelope) {
            return Err((error, envelope));
        }
        if matches!(&envelope.event, ActiveExecutionEvent::Exited(_)) {
            mark_execute_exit_event_queued(&envelope.vm_id, &envelope.process_id);
        }
        self.pending_process_events.push_back(envelope);
        self.observe_pending_process_event_depth();
        Ok(())
    }

    pub(crate) fn process_event_broker(&self) -> ProcessEventBroker {
        self.process_event_broker.clone()
    }

    /// Transfer at most one raw event for an ownership-validated process into
    /// the independent broker. Pumping and registry access are short-lived;
    /// the caller awaits its broker lease after this method releases `&mut
    /// NativeSidecar`.
    pub(crate) async fn route_owned_process_event_to_broker(
        &mut self,
        ownership: OwnershipScope,
        target: ProcessEventTarget,
    ) -> Result<bool, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        if target.connection_id != connection_id
            || target.session_id != session_id
            || target.vm_id != vm_id
        {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_PROCESS_EVENT_OWNERSHIP: target {}/{}/{} is not owned by {connection_id}/{session_id}/{vm_id}",
                target.connection_id, target.session_id, target.vm_id
            )));
        }
        self.process_event_broker
            .claim_target(&ownership, target.clone())
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        self.pump_process_events(&ownership).await?;
        let Some(envelope) =
            self.take_matching_process_event_envelope(&target.vm_id, &target.process_id)?
        else {
            return Ok(false);
        };
        match self.process_event_ingress.try_publish(envelope) {
            Ok(()) => Ok(true),
            Err(failure) => {
                // Preserve ordering and ownership if broker admission loses a
                // race with another target. The caller gets the typed limit and
                // can retry after capacity is released.
                self.queue_front_pending_process_event(failure.envelope)?;
                Err(SidecarError::InvalidState(failure.error.to_string()))
            }
        }
    }

    /// One bounded broker handoff without polling or awaiting a runtime. The
    /// coalesced process-event supervisor owns runtime polling; extension
    /// waiters merely route already-durable events here.
    pub(crate) fn route_owned_process_event_to_broker_nowait(
        &mut self,
        ownership: &OwnershipScope,
        target: &ProcessEventTarget,
    ) -> Result<bool, SidecarError> {
        let (connection_id, session_id, vm_id) = self.vm_scope_for(ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        if target.connection_id != connection_id
            || target.session_id != session_id
            || target.vm_id != vm_id
        {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_PROCESS_EVENT_OWNERSHIP: target {}/{}/{} is not owned by {connection_id}/{session_id}/{vm_id}",
                target.connection_id, target.session_id, target.vm_id
            )));
        }
        self.process_event_broker
            .claim_target(ownership, target.clone())
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        let Some(envelope) =
            self.take_matching_process_event_envelope(&target.vm_id, &target.process_id)?
        else {
            return Ok(false);
        };
        match self.process_event_ingress.try_publish(envelope) {
            Ok(()) => Ok(true),
            Err(failure) => {
                self.queue_front_pending_process_event(failure.envelope)?;
                Err(SidecarError::InvalidState(failure.error.to_string()))
            }
        }
    }

    /// Move every currently retained claimed-target envelope into the broker in
    /// the same coordinator turn that drained its producer. This is the
    /// durable handoff for the competing stdio wake consumer: whichever branch
    /// receives `process_event_notify` routes claimed adapter output and wakes
    /// the exact broker waiter.
    pub(crate) fn route_claimed_pending_process_events(&mut self) -> Result<usize, SidecarError> {
        let mut routed = 0usize;
        let mut index = 0usize;
        while index < self.pending_process_events.len() {
            let claimed = {
                let envelope = &self.pending_process_events[index];
                let target = ProcessEventTarget::new(
                    envelope.connection_id.clone(),
                    envelope.session_id.clone(),
                    envelope.vm_id.clone(),
                    envelope.process_id.clone(),
                )
                .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
                self.process_event_broker
                    .target_is_claimed(&target)
                    .map_err(|error| SidecarError::InvalidState(error.to_string()))?
            };
            if !claimed {
                index += 1;
                continue;
            }
            let envelope = self
                .pending_process_events
                .remove(index)
                .expect("claimed pending process event index");
            match self.process_event_ingress.try_publish(envelope) {
                Ok(()) => routed = routed.saturating_add(1),
                Err(failure) => {
                    self.pending_process_events.insert(index, failure.envelope);
                    self.observe_pending_process_event_depth();
                    return match failure.error {
                        ProcessEventBrokerError::Limit { .. } => Ok(routed),
                        error => Err(SidecarError::InvalidState(error.to_string())),
                    };
                }
            }
        }
        if routed > 0 {
            self.observe_pending_process_event_depth();
            self.rearm_deferred_process_event_after_capacity_release();
        }
        Ok(routed)
    }

    pub(crate) fn queue_front_pending_process_event(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<(), SidecarError> {
        self.check_pending_process_event_capacity(&envelope)?;
        if matches!(&envelope.event, ActiveExecutionEvent::Exited(_)) {
            mark_execute_exit_event_queued(&envelope.vm_id, &envelope.process_id);
        }
        self.pending_process_events.push_front(envelope);
        self.observe_pending_process_event_depth();
        Ok(())
    }

    pub(crate) fn pending_process_event_capacity(&self) -> usize {
        self.config
            .runtime
            .protocol
            .max_process_events
            .saturating_sub(
                self.pending_process_events
                    .len()
                    .saturating_add(usize::from(self.deferred_process_event_envelope.is_some())),
            )
    }

    pub(crate) fn check_pending_process_event_capacity(
        &self,
        envelope: &ProcessEventEnvelope,
    ) -> Result<(), SidecarError> {
        self.validate_process_event_envelope_locator(envelope)?;
        let global_limit = self.config.runtime.protocol.max_process_events;
        if self
            .pending_process_events
            .len()
            .saturating_add(usize::from(self.deferred_process_event_envelope.is_some()))
            >= global_limit
        {
            return Err(process_event_queue_overflow_error(global_limit));
        }
        let limits = self
            .vms
            .get(&envelope.vm_id)
            .map(|vm| vm.limits.process.clone())
            .unwrap_or_default();
        let mut vm_count = 0usize;
        let mut vm_bytes = 0usize;
        for pending in self
            .pending_process_events
            .iter()
            .chain(self.deferred_process_event_envelope.iter())
            .filter(|pending| pending.vm_id == envelope.vm_id)
        {
            vm_count = vm_count.saturating_add(1);
            vm_bytes = vm_bytes.saturating_add(pending.retained_bytes());
        }
        if vm_count >= limits.pending_event_count {
            return Err(SidecarError::InvalidState(format!(
                "VM {} process event queue exceeded {} events (limits.process.pendingEventCount)",
                envelope.vm_id, limits.pending_event_count
            )));
        }
        let next_bytes = vm_bytes.saturating_add(envelope.retained_bytes());
        if next_bytes > limits.pending_event_bytes {
            return Err(SidecarError::InvalidState(format!(
                "VM {} process event queue exceeded {} retained bytes (limits.process.pendingEventBytes)",
                envelope.vm_id, limits.pending_event_bytes
            )));
        }
        Ok(())
    }

    pub(crate) fn validate_process_event_envelope_locator(
        &self,
        envelope: &ProcessEventEnvelope,
    ) -> Result<(), SidecarError> {
        let global_limit = self.config.runtime.protocol.max_process_events;
        if envelope.child_path.len() > global_limit {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_PROCESS_EVENT_PATH_LIMIT: process-event child path has {} segments, exceeding runtime.protocol.maxProcessEvents ({global_limit}); raise runtime.protocol.maxProcessEvents",
                envelope.child_path.len()
            )));
        }
        let child_path_bytes = envelope
            .child_path
            .iter()
            .try_fold(0usize, |total, segment| {
                if segment.len() > self.config.max_frame_bytes {
                    return Err(SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_PROCESS_EVENT_PATH_LIMIT: process-event child path segment has {} bytes, exceeding maxFrameBytes ({}); raise maxFrameBytes",
                        segment.len(), self.config.max_frame_bytes
                    )));
                }
                Ok(total.saturating_add(segment.len()))
            })?;
        if child_path_bytes > self.config.max_frame_bytes {
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_PROCESS_EVENT_PATH_LIMIT: process-event child path has {child_path_bytes} bytes, exceeding maxFrameBytes ({}); raise maxFrameBytes",
                self.config.max_frame_bytes
            )));
        }
        Ok(())
    }

    pub(crate) fn observe_pending_process_event_depth(&self) {
        self.pending_process_events_gauge
            .observe_depth(self.pending_process_events.len());
        self.pending_process_event_bytes_gauge.observe_depth(
            self.pending_process_events
                .iter()
                .chain(self.deferred_process_event_envelope.iter())
                .fold(0usize, |bytes, event| {
                    bytes.saturating_add(event.retained_bytes())
                }),
        );
    }

    /// A deferred receiver envelope is retried only after a consumer releases
    /// public queue capacity. Never call this when initially staging it: that
    /// would turn a stable full queue into a notification hot-spin.
    pub(crate) fn rearm_deferred_process_event_after_capacity_release(&self) {
        if self.deferred_process_event_envelope.is_some() {
            self.process_event_notify.notify_one();
        }
    }

    pub fn dispatch_blocking(
        &mut self,
        request: RequestFrame,
    ) -> Result<DispatchResult, SidecarError> {
        let inside_runtime = tokio::runtime::Handle::try_current().is_ok();
        if !inside_runtime {
            let handle = self.process_runtime_handle()?;
            return handle.block_on(self.dispatch(request));
        }

        let mut future = std::pin::pin!(self.dispatch(request));
        match poll_future_once(future.as_mut()) {
            Some(result) => result,
            None => Err(SidecarError::InvalidState(String::from(
                "dispatch_blocking cannot wait for an async sidecar request inside a Tokio runtime; use dispatch().await",
            ))),
        }
    }

    pub fn dispatch_wire_blocking(
        &mut self,
        request: crate::wire::RequestFrame,
    ) -> Result<crate::wire::WireDispatchResult, SidecarError> {
        let request = crate::wire::request_frame_to_compat(request).map_err(wire_protocol_error)?;
        let result = self.dispatch_blocking(request)?;
        wire_dispatch_result(result)
    }

    pub fn poll_event_blocking(
        &mut self,
        ownership: &OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let handle = self.process_runtime_handle()?;
        handle.block_on(self.poll_event(ownership, timeout))
    }

    pub fn poll_event_wire_blocking(
        &mut self,
        ownership: &crate::wire::OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<crate::wire::EventFrame>, SidecarError> {
        let ownership = crate::wire::ownership_scope_to_compat(ownership.clone());
        self.poll_event_blocking(&ownership, timeout)?
            .map(crate::wire::event_frame_from_compat)
            .transpose()
            .map_err(wire_protocol_error)
    }

    pub fn close_session_blocking(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        let handle = self.process_runtime_handle()?;
        handle.block_on(self.close_session(connection_id, session_id))
    }

    pub fn remove_connection_blocking(
        &mut self,
        connection_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        let handle = self.process_runtime_handle()?;
        handle.block_on(self.remove_connection(connection_id))
    }

    pub fn dispose_vm_internal_blocking(
        &mut self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        let handle = self.process_runtime_handle()?;
        handle.block_on(self.dispose_vm_internal(connection_id, session_id, vm_id, reason))
    }

    fn process_runtime_handle(&self) -> Result<tokio::runtime::Handle, SidecarError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(SidecarError::InvalidState(String::from(
                "blocking sidecar API cannot run on a Tokio worker; use the async API",
            )));
        }
        self.runtime_context
            .as_ref()
            .map(|context| context.handle().clone())
            .ok_or_else(|| {
                SidecarError::InvalidState(String::from(
                    "blocking sidecar API requires the process RuntimeContext; construct with with_config_extensions_and_runtime or use the async API",
                ))
            })
    }

    pub async fn dispatch(
        &mut self,
        request: RequestFrame,
    ) -> Result<DispatchResult, SidecarError> {
        self.reap_reconciled_quarantined_vms();
        if let Err(error) = self.ensure_request_within_frame_limit(&request) {
            return Ok(DispatchResult {
                response: self.reject_error(&request, &error),
                events: Vec::new(),
            });
        }

        let route = route_request_payload(&request);
        if !matches!(&route, RequestRoute::DisposeVm(_)) {
            if let OwnershipScope::VmOwnership(ownership) = &request.ownership {
                if let Some(report) = self
                    .vms
                    .get(&ownership.vm_id)
                    .and_then(|vm| vm.runtime_context.terminal_failure())
                {
                    let error = SidecarError::Execution(format!(
                        "ERR_AGENTOS_VM_TASK_FAILED: vm_id={} class={:?} owner={} reason={:?}; dispose and recreate this VM generation",
                        ownership.vm_id, report.class, report.owner, report.reason
                    ));
                    return Ok(DispatchResult {
                        response: self.reject_error(&request, &error),
                        events: Vec::new(),
                    });
                }
            }
        }

        let result = match route {
            RequestRoute::Authenticate(payload) => self.authenticate_connection(&request, payload),
            RequestRoute::OpenSession(payload) => self.open_session(&request, payload),
            RequestRoute::CreateVm(payload) => self.create_vm(&request, payload).await,
            RequestRoute::DisposeVm(payload) => self.dispose_vm(&request, payload).await,
            RequestRoute::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload.entries)
                    .await
            }
            RequestRoute::ConfigureVm(payload) => self.configure_vm(&request, payload).await,
            RequestRoute::RegisterHostCallbacks(payload) => {
                register_host_callbacks(self, &request, payload).await
            }
            RequestRoute::CreateLayer(payload) => self.create_layer(&request, payload).await,
            RequestRoute::SealLayer(payload) => self.seal_layer(&request, payload).await,
            RequestRoute::ImportSnapshot(payload) => self.import_snapshot(&request, payload).await,
            RequestRoute::ExportSnapshot(payload) => self.export_snapshot(&request, payload).await,
            RequestRoute::CreateOverlay(payload) => self.create_overlay(&request, payload).await,
            RequestRoute::GuestFilesystemCall(payload) => {
                self.guest_filesystem_call(&request, payload).await
            }
            RequestRoute::GuestKernelCall(payload) => {
                self.guest_kernel_call(&request, payload).await
            }
            RequestRoute::SnapshotRootFilesystem(payload) => {
                self.snapshot_root_filesystem(&request, payload).await
            }
            RequestRoute::ListMounts(payload) => self.list_mounts(&request, payload).await,
            RequestRoute::ExecutionOperation(payload) => {
                let effects = self.request_completion_effects(&request);
                let result = self
                    .execute_language_operation(&request, payload, effects.clone())
                    .await;
                self.apply_request_completion_effects(&effects);
                result
            }
            RequestRoute::ExecutionLifecycle(payload) => {
                let effects = self.request_completion_effects(&request);
                let result = self
                    .handle_execution_lifecycle(&request, payload, effects.clone())
                    .await;
                self.apply_request_completion_effects(&effects);
                result
            }
            RequestRoute::Execute(payload) => self.execute(&request, payload).await,
            RequestRoute::WriteStdin(payload) => self.write_stdin(&request, payload).await,
            RequestRoute::ResizePty(payload) => self.resize_pty(&request, payload).await,
            RequestRoute::CloseStdin(payload) => self.close_stdin(&request, payload).await,
            RequestRoute::KillProcess(payload) => self.kill_process(&request, payload).await,
            RequestRoute::GetProcessSnapshot(payload) => {
                self.get_process_snapshot(&request, payload).await
            }
            RequestRoute::GetResourceSnapshot(payload) => {
                self.get_resource_snapshot(&request, payload).await
            }
            RequestRoute::FindListener(payload) => self.find_listener(&request, payload).await,
            RequestRoute::FindBoundUdp(payload) => self.find_bound_udp(&request, payload).await,
            RequestRoute::VmFetch(payload) => self.vm_fetch(&request, payload).await,
            RequestRoute::GetSignalState(payload) => self.get_signal_state(&request, payload).await,
            RequestRoute::GetZombieTimerCount(payload) => {
                self.get_zombie_timer_count(&request, payload).await
            }
            RequestRoute::LinkPackage(payload) => self.link_package(&request, payload).await,
            RequestRoute::ProvidedCommands(payload) => {
                self.provided_commands(&request, payload).await
            }
            RequestRoute::UnsupportedHostCallbackDirection => {
                Ok(unsupported_host_callback_direction_dispatch(&request))
            }
            RequestRoute::Ext(payload) => self.dispatch_extension_request(&request, payload).await,
        };

        match result {
            Ok(dispatch) => Ok(dispatch),
            Err(error @ SidecarError::Io(_)) => Err(error),
            Err(error) => Ok(DispatchResult {
                response: self.reject_error(&request, &error),
                events: Vec::new(),
            }),
        }
    }

    pub async fn dispatch_wire(
        &mut self,
        request: crate::wire::RequestFrame,
    ) -> Result<crate::wire::WireDispatchResult, SidecarError> {
        let request = crate::wire::request_frame_to_compat(request).map_err(wire_protocol_error)?;
        let result = self.dispatch(request).await?;
        wire_dispatch_result(result)
    }

    /// Prepare a non-extension request that can finish without retaining the
    /// process coordinator. Returns `Ok(None)` when the route still requires
    /// entity-owned mutable state and must use the coordinated dispatch path.
    ///
    /// Immutable queries are snapshotted synchronously. Mutable VM filesystem
    /// and kernel calls clone only their VM handle and execute under that VM's
    /// short critical section, so neither route retains the process
    /// coordinator or blocks operations owned by another VM.
    pub(crate) fn prepare_request_wire(
        &mut self,
        request: crate::wire::RequestFrame,
    ) -> Result<Option<PreparedRequest>, SidecarError> {
        let request = crate::wire::request_frame_to_compat(request).map_err(wire_protocol_error)?;
        let route = route_request_payload(&request);
        let detachable = matches!(
            &route,
            RequestRoute::Authenticate(_)
                | RequestRoute::OpenSession(_)
                | RequestRoute::RegisterHostCallbacks(_)
                | RequestRoute::GetProcessSnapshot(_)
                | RequestRoute::GetResourceSnapshot(_)
                | RequestRoute::GetZombieTimerCount(_)
                | RequestRoute::ProvidedCommands(_)
                | RequestRoute::ListMounts(_)
                | RequestRoute::GuestFilesystemCall(_)
                | RequestRoute::GuestKernelCall(_)
                | RequestRoute::BootstrapRootFilesystem(_)
                | RequestRoute::ConfigureVm(_)
                | RequestRoute::CreateLayer(_)
                | RequestRoute::SealLayer(_)
                | RequestRoute::ImportSnapshot(_)
                | RequestRoute::ExportSnapshot(_)
                | RequestRoute::CreateOverlay(_)
                | RequestRoute::SnapshotRootFilesystem(_)
                | RequestRoute::LinkPackage(_)
                | RequestRoute::Execute(_)
                | RequestRoute::ExecutionOperation(_)
                | RequestRoute::ExecutionLifecycle(_)
                | RequestRoute::WriteStdin(_)
                | RequestRoute::ResizePty(_)
                | RequestRoute::CloseStdin(_)
                | RequestRoute::KillProcess(_)
                | RequestRoute::FindListener(_)
                | RequestRoute::FindBoundUdp(_)
                | RequestRoute::VmFetch(_)
                | RequestRoute::GetSignalState(_)
                | RequestRoute::UnsupportedHostCallbackDirection
        ) || matches!(
            &route,
            RequestRoute::Ext(envelope) if !self.extensions.contains_key(&envelope.namespace)
        );
        if !detachable {
            return Ok(None);
        }

        self.reap_reconciled_quarantined_vms();
        let preparation = (|| {
            self.ensure_request_within_frame_limit(&request)?;
            if let OwnershipScope::VmOwnership(ownership) = &request.ownership {
                if let Some(report) = self
                    .vms
                    .get(&ownership.vm_id)
                    .and_then(|vm| vm.runtime_context.terminal_failure())
                {
                    return Err(SidecarError::Execution(format!(
                        "ERR_AGENTOS_VM_TASK_FAILED: vm_id={} class={:?} owner={} reason={:?}; dispose and recreate this VM generation",
                        ownership.vm_id, report.class, report.owner, report.reason
                    )));
                }
            }
            Ok(())
        })();
        if let Err(error) = preparation {
            return Ok(Some(PreparedRequest::from_future(request, async move {
                Err(error)
            })));
        }

        match route {
            RequestRoute::Authenticate(payload) => {
                if let Err(error) = self.connection_id_for(&request.ownership) {
                    return Ok(Some(PreparedRequest::failed(request, error)));
                }
                if let Err(error) = self.validate_auth_token(&payload.auth_token) {
                    let bridge = self.bridge.clone();
                    let sidecar_id = self.config.sidecar_id.clone();
                    let client_name = payload.client_name;
                    let ownership = request.ownership.clone();
                    return Ok(Some(PreparedRequest::from_future(request, async move {
                        let mut fields = audit_fields([
                            (String::from("source"), client_name),
                            (String::from("reason"), error.to_string()),
                        ]);
                        if let OwnershipScope::ConnectionOwnership(inner) = ownership {
                            fields.insert(String::from("connection_id"), inner.connection_id);
                        }
                        emit_security_audit_event(
                            &bridge,
                            &sidecar_id,
                            "security.auth.failed",
                            fields,
                        );
                        Err(error)
                    })));
                }
                if let Err(error) =
                    validate_authenticate_versions(&payload).map_err(|error| match error {
                        AuthenticateVersionError::ProtocolVersionMismatch(message) => {
                            SidecarError::ProtocolVersionMismatch(message)
                        }
                        AuthenticateVersionError::BridgeVersionMismatch(message) => {
                            SidecarError::BridgeVersionMismatch(message)
                        }
                    })
                {
                    return Ok(Some(PreparedRequest::failed(request, error)));
                }
                let Some(connection_number) = self.next_connection_id.checked_add(1) else {
                    return Ok(Some(PreparedRequest::failed(
                        request,
                        SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_CONNECTION_ID_EXHAUSTED: connection identifier space exhausted",
                        )),
                    )));
                };
                let connection_id = format!("conn-{connection_number}");
                let response = shared_authenticated_response(
                    request.request_id,
                    self.config.sidecar_id.clone(),
                    connection_id.clone(),
                    self.config.max_frame_bytes as u32,
                );
                let membership = PreparedMembershipCommit::Connection {
                    connection_id,
                    auth_token: payload.auth_token,
                };
                return Ok(Some(PreparedRequest::from_future_with_membership(
                    request,
                    async move {
                        Ok(DispatchResult {
                            response,
                            events: Vec::new(),
                        })
                    },
                    membership,
                )));
            }
            RequestRoute::OpenSession(payload) => {
                let connection_id = match self.connection_id_for(&request.ownership) {
                    Ok(connection_id) => connection_id,
                    Err(error) => return Ok(Some(PreparedRequest::failed(request, error))),
                };
                if let Err(error) = self.require_authenticated_connection(&connection_id) {
                    return Ok(Some(PreparedRequest::failed(request, error)));
                }
                let Some(session_number) = self.next_session_id.checked_add(1) else {
                    return Ok(Some(PreparedRequest::failed(
                        request,
                        SidecarError::InvalidState(String::from(
                            "ERR_AGENTOS_SESSION_ID_EXHAUSTED: session identifier space exhausted",
                        )),
                    )));
                };
                let session_id = format!("session-{session_number}");
                let response = session_opened_response(
                    request.request_id,
                    connection_id.clone(),
                    session_id.clone(),
                );
                let membership = PreparedMembershipCommit::Session {
                    connection_id,
                    session_id,
                    placement: payload.placement,
                    metadata: payload.metadata.into_iter().collect(),
                };
                return Ok(Some(PreparedRequest::from_future_with_membership(
                    request,
                    async move {
                        Ok(DispatchResult {
                            response,
                            events: Vec::new(),
                        })
                    },
                    membership,
                )));
            }
            RequestRoute::RegisterHostCallbacks(payload) => {
                let future = register_host_callbacks(self, &request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::GetProcessSnapshot(payload) => {
                let future = self.get_process_snapshot(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::GetResourceSnapshot(payload) => {
                let future = self.get_resource_snapshot(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::GetZombieTimerCount(payload) => {
                let future = self.get_zombie_timer_count(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ProvidedCommands(payload) => {
                let future = self.provided_commands(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ListMounts(payload) => {
                let future = self.list_mounts(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::BootstrapRootFilesystem(payload) => {
                let future = self.bootstrap_root_filesystem(&request, payload.entries);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ConfigureVm(payload) => {
                let future = self.configure_vm(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::CreateLayer(payload) => {
                let future = self.create_layer(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::SealLayer(payload) => {
                let future = self.seal_layer(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ImportSnapshot(payload) => {
                let future = self.import_snapshot(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ExportSnapshot(payload) => {
                let future = self.export_snapshot(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::CreateOverlay(payload) => {
                let future = self.create_overlay(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::SnapshotRootFilesystem(payload) => {
                let future = self.snapshot_root_filesystem(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::LinkPackage(payload) => {
                let future = self.link_package(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::Execute(payload) => {
                let future = self.execute(&request, payload);
                return Ok(Some(PreparedRequest::from_future(request, future)));
            }
            RequestRoute::ExecutionOperation(payload) => {
                let effects = self.request_completion_effects(&request);
                let future = self.execute_language_operation(&request, payload, effects.clone());
                return Ok(Some(PreparedRequest::from_future_with_effects(
                    request, future, effects,
                )));
            }
            RequestRoute::ExecutionLifecycle(payload) => {
                let effects = self.request_completion_effects(&request);
                let future = self.handle_execution_lifecycle(&request, payload, effects.clone());
                return Ok(Some(PreparedRequest::from_future_with_effects(
                    request, future, effects,
                )));
            }
            RequestRoute::WriteStdin(payload) => {
                let future = self.write_stdin(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::ResizePty(payload) => {
                let future = self.resize_pty(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::CloseStdin(payload) => {
                let future = self.close_stdin(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::KillProcess(payload) => {
                let future = self.kill_process(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::FindListener(payload) => {
                let future = self.find_listener(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::FindBoundUdp(payload) => {
                let future = self.find_bound_udp(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::VmFetch(payload) => {
                let future = self.vm_fetch(&request, payload);
                return Ok(Some(PreparedRequest::from_future(request, future)));
            }
            RequestRoute::GetSignalState(payload) => {
                let future = self.get_signal_state(&request, payload);
                return Ok(Some(PreparedRequest::from_vm_command(request, future)));
            }
            RequestRoute::GuestFilesystemCall(payload) => {
                let (connection_id, session_id, vm_id) = match self.vm_scope_for(&request.ownership)
                {
                    Ok(scope) => scope,
                    Err(error) => return Ok(Some(PreparedRequest::failed(request, error))),
                };
                if let Err(error) = self.require_owned_vm(&connection_id, &session_id, &vm_id) {
                    return Ok(Some(PreparedRequest::failed(request, error)));
                }
                let Some(handle) = self.vms.handle(&vm_id) else {
                    return Ok(Some(PreparedRequest::failed(
                        request,
                        SidecarError::InvalidState(format!(
                            "VM {vm_id} no longer exists for guest filesystem call"
                        )),
                    )));
                };
                let response_request = request.clone();
                return Ok(Some(PreparedRequest::from_vm_command(
                    request,
                    async move {
                        let response = handle.try_command("guest filesystem call", |vm| {
                            guest_filesystem_call_vm(vm, &payload)
                        })?;
                        Ok(DispatchResult {
                            response: shared_respond(
                                &response_request,
                                ResponsePayload::GuestFilesystemResult(response),
                            ),
                            events: Vec::new(),
                        })
                    },
                )));
            }
            RequestRoute::GuestKernelCall(payload) => {
                let (connection_id, session_id, vm_id) = match self.vm_scope_for(&request.ownership)
                {
                    Ok(scope) => scope,
                    Err(error) => return Ok(Some(PreparedRequest::failed(request, error))),
                };
                if let Err(error) = self.require_owned_vm(&connection_id, &session_id, &vm_id) {
                    return Ok(Some(PreparedRequest::failed(request, error)));
                }
                let Some(handle) = self.vms.handle(&vm_id) else {
                    return Ok(Some(PreparedRequest::failed(
                        request,
                        SidecarError::InvalidState(format!(
                            "VM {vm_id} no longer exists for guest kernel call"
                        )),
                    )));
                };
                let response_request = request.clone();
                return Ok(Some(PreparedRequest::from_vm_command(
                    request,
                    async move {
                        let response = handle.try_command("guest kernel call", |vm| {
                            let kernel_pid = vm
                                .active_processes
                                .get(&payload.execution_id)
                                .map(|process| process.kernel_pid)
                                .ok_or_else(|| {
                                    SidecarError::InvalidState(format!(
                                        "VM {vm_id} has no active process {} for guest kernel call",
                                        payload.execution_id
                                    ))
                                })?;
                            agentos_native_sidecar_core::handle_guest_kernel_call(
                                &mut vm.kernel,
                                kernel_pid,
                                EXECUTION_DRIVER_NAME,
                                &payload.operation,
                                &payload.payload,
                            )
                            .map_err(crate::execution::guest_kernel_core_error)
                        })?;
                        Ok(DispatchResult {
                            response: shared_respond(
                                &response_request,
                                ResponsePayload::GuestKernelResult(
                                    crate::protocol::GuestKernelResultResponse {
                                        payload: response,
                                    },
                                ),
                            ),
                            events: Vec::new(),
                        })
                    },
                )));
            }
            RequestRoute::UnsupportedHostCallbackDirection => {
                let response_request = request.clone();
                return Ok(Some(PreparedRequest::from_future(request, async move {
                    Ok(unsupported_host_callback_direction_dispatch(
                        &response_request,
                    ))
                })));
            }
            RequestRoute::Ext(envelope) => {
                let response_request = request.clone();
                return Ok(Some(PreparedRequest::from_future(request, async move {
                    Ok(DispatchResult {
                        response: shared_reject(
                            &response_request,
                            "unknown_extension",
                            &format!(
                                "no extension registered for namespace {}",
                                envelope.namespace
                            ),
                        ),
                        events: Vec::new(),
                    })
                })));
            }
            RequestRoute::CreateVm(_) | RequestRoute::DisposeVm(_) => {
                unreachable!("VM creation and disposal use dedicated prepared routes")
            }
        }
    }

    /// Finalize a detached non-extension request without awaiting business work.
    fn request_completion_effects(&self, request: &RequestFrame) -> RequestCompletionEffects {
        let limit = match &request.ownership {
            OwnershipScope::VmOwnership(ownership) => self
                .vms
                .get(&ownership.vm_id)
                .map(|vm| {
                    vm.limits
                        .execution
                        .max_completed_executions
                        .saturating_add(1)
                })
                .unwrap_or(1),
            OwnershipScope::ConnectionOwnership(_) | OwnershipScope::SessionOwnership(_) => 1,
        };
        RequestCompletionEffects::new(limit, "limits.execution.maxCompletedExecutions")
    }

    fn apply_request_completion_effects(&mut self, effects: &RequestCompletionEffects) {
        for process_id in effects.take_exited_processes() {
            self.prune_extension_process_resource(&process_id);
        }
    }

    pub(crate) fn complete_request(
        &mut self,
        completed: CompletedRequest,
    ) -> Result<crate::wire::WireDispatchResult, SidecarError> {
        let CompletedRequest {
            request,
            result,
            effects,
            committed_membership,
        } = completed;
        if result.is_ok() {
            if let Some(membership) = committed_membership.as_ref() {
                self.commit_prepared_membership(membership)?;
            }
        }
        self.apply_request_completion_effects(&effects);
        let dispatch = match result {
            Ok(dispatch) => dispatch,
            Err(error @ SidecarError::Io(_)) => return Err(error),
            Err(error) => DispatchResult {
                response: self.reject_error(&request, &error),
                events: Vec::new(),
            },
        };
        wire_dispatch_result(dispatch)
    }

    pub async fn poll_event_wire(
        &mut self,
        ownership: &crate::wire::OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<crate::wire::EventFrame>, SidecarError> {
        let ownership = crate::wire::ownership_scope_to_compat(ownership.clone());
        self.poll_event(&ownership, timeout)
            .await?
            .map(crate::wire::event_frame_from_compat)
            .transpose()
            .map_err(wire_protocol_error)
    }

    /// Detach an extension request from the process coordinator so its long
    /// wait can run under the request supervisor. Returns `Ok(None)` for
    /// non-extension and unknown-extension requests; those retain the normal
    /// dispatch path, including its canonical unknown-extension rejection.
    pub(crate) fn prepare_extension_request_wire(
        &self,
        request: crate::wire::RequestFrame,
        services: Arc<dyn ExtensionServices>,
    ) -> Result<Option<PreparedExtensionRequest>, SidecarError> {
        let request = crate::wire::request_frame_to_compat(request).map_err(wire_protocol_error)?;
        let RequestPayload::Ext(envelope) = &request.payload else {
            return Ok(None);
        };
        let Some(extension) = self.extensions.get(&envelope.namespace).cloned() else {
            return Ok(None);
        };
        let namespace = envelope.namespace.clone();
        let payload = envelope.payload.clone();
        let snapshot = ExtensionSnapshot::new(
            namespace.clone(),
            request.ownership.clone(),
            self.sidecar_requests.clone(),
            self.event_sink.clone(),
        );
        Ok(Some(PreparedExtensionRequest {
            request,
            namespace,
            payload,
            extension,
            services,
            snapshot,
        }))
    }

    /// Convert a detached extension task's result into the same canonical wire
    /// response used by inline dispatch. This is deliberately short and never
    /// waits on extension work or output capacity.
    pub(crate) fn complete_extension_request(
        &self,
        completed: CompletedExtensionRequest,
    ) -> Result<crate::wire::WireDispatchResult, SidecarError> {
        let CompletedExtensionRequest {
            request,
            namespace,
            result,
        } = completed;
        let dispatch = match result {
            Ok(response) => DispatchResult {
                response: self.respond(
                    &request,
                    ResponsePayload::ExtResult(ExtEnvelope {
                        namespace,
                        payload: response.payload,
                    }),
                ),
                events: response.events,
            },
            Err(error @ SidecarError::Io(_)) => return Err(error),
            Err(error) => DispatchResult {
                response: self.reject_error(&request, &error),
                events: Vec::new(),
            },
        };
        wire_dispatch_result(dispatch)
    }

    pub(crate) fn reject_wire_request_error(
        &self,
        request: crate::wire::RequestFrame,
        error: &SidecarError,
    ) -> Result<crate::wire::WireDispatchResult, SidecarError> {
        let request = crate::wire::request_frame_to_compat(request).map_err(wire_protocol_error)?;
        wire_dispatch_result(DispatchResult {
            response: self.reject_error(&request, error),
            events: Vec::new(),
        })
    }

    async fn dispatch_extension_request(
        &mut self,
        request: &RequestFrame,
        envelope: ExtEnvelope,
    ) -> Result<DispatchResult, SidecarError> {
        let namespace = envelope.namespace;
        let Some(extension) = self.extensions.get(&namespace).cloned() else {
            return Ok(DispatchResult {
                response: self.reject(
                    request,
                    "unknown_extension",
                    &format!("no extension registered for namespace {namespace}"),
                ),
                events: Vec::new(),
            });
        };
        let snapshot = ExtensionSnapshot::new(
            namespace.clone(),
            request.ownership.clone(),
            self.sidecar_requests.clone(),
            self.event_sink.clone(),
        );
        let services = self.extension_services.clone().ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "ERR_AGENTOS_EXTENSION_SERVICES_UNAVAILABLE: extension dispatch requires cloneable owned services",
            ))
        })?;
        let ctx = ExtensionContext::with_services(snapshot, services);
        let response = extension.handle_request(ctx, envelope.payload).await?;
        Ok(DispatchResult {
            response: self.respond(
                request,
                ResponsePayload::ExtResult(ExtEnvelope {
                    namespace,
                    payload: response.payload,
                }),
            ),
            events: response.events,
        })
    }

    pub async fn poll_event(
        &mut self,
        ownership: &OwnershipScope,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let deadline = Instant::now() + timeout;
        let process_event_notify = Arc::clone(&self.process_event_notify);
        loop {
            // Register before probing durable queues so a producer racing the
            // probe cannot lose its edge between the empty check and await.
            let notified = process_event_notify.notified();
            if let Some(index) = self
                .pending_process_events
                .iter()
                .position(|event| public_process_event_matches_ownership(self, ownership, event))
            {
                let Some(envelope) = self.pending_process_events.remove(index) else {
                    continue;
                };
                self.observe_pending_process_event_depth();
                self.rearm_deferred_process_event_after_capacity_release();
                if let Some(frame) = self.handle_process_event_envelope(envelope).await? {
                    return Ok(Some(frame));
                }
                continue;
            }

            if !timeout.is_zero() && self.pump_process_events(ownership).await? {
                // The pump moves execution events into durable sidecar queues.
                // Re-probe those queues before waiting for another edge: the
                // notification that brought us here may be the only edge for
                // this event, and waiting now would strand it until unrelated
                // later activity.
                continue;
            }

            let queued_envelopes = {
                let pending_capacity = self.pending_process_event_capacity();
                let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("process event receiver unavailable"))
                })?;
                let mut queued = Vec::new();
                loop {
                    if queued.len() >= pending_capacity {
                        if receiver.is_empty() {
                            break;
                        }
                        return Err(process_event_queue_overflow_error(
                            self.config.runtime.protocol.max_process_events,
                        ));
                    }
                    match receiver.try_recv() {
                        Ok(envelope) => queued.push(envelope),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }
                queued
            };

            let mut matching_envelope = None;
            for envelope in queued_envelopes {
                if matching_envelope.is_none()
                    && public_process_event_matches_ownership(self, ownership, &envelope)
                {
                    matching_envelope = Some(envelope);
                } else {
                    self.queue_pending_process_event(envelope)?;
                }
            }

            if let Some(envelope) = matching_envelope {
                if let Some(frame) = self.handle_process_event_envelope(envelope).await? {
                    return Ok(Some(frame));
                }
                continue;
            }

            if Instant::now() >= deadline {
                return Ok(None);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::select! {
                _ = notified => {}
                _ = time::sleep(remaining) => return Ok(None),
            }
        }
    }

    /// Probe durable public event queues once without awaiting runtime work.
    /// Runtime polling is owned by the coalesced process-event supervisor.
    pub(crate) fn poll_event_nowait(
        &mut self,
        ownership: &OwnershipScope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        loop {
            if let Some(index) = self
                .pending_process_events
                .iter()
                .position(|event| public_process_event_matches_ownership(self, ownership, event))
            {
                let Some(envelope) = self.pending_process_events.remove(index) else {
                    continue;
                };
                self.observe_pending_process_event_depth();
                self.rearm_deferred_process_event_after_capacity_release();
                if let Some(frame) = self.handle_public_process_event_envelope_nowait(envelope)? {
                    return Ok(Some(frame));
                }
                continue;
            }

            let queued_envelopes = {
                let pending_capacity = self.pending_process_event_capacity();
                let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                    SidecarError::InvalidState(String::from("process event receiver unavailable"))
                })?;
                let mut queued = Vec::new();
                loop {
                    if queued.len() >= pending_capacity {
                        if receiver.is_empty() {
                            break;
                        }
                        return Err(process_event_queue_overflow_error(
                            self.config.runtime.protocol.max_process_events,
                        ));
                    }
                    match receiver.try_recv() {
                        Ok(envelope) => queued.push(envelope),
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                    }
                }
                queued
            };
            let mut matching = None;
            for envelope in queued_envelopes {
                if matching.is_none()
                    && public_process_event_matches_ownership(self, ownership, &envelope)
                {
                    matching = Some(envelope);
                } else {
                    self.queue_pending_process_event(envelope)?;
                }
            }
            let Some(envelope) = matching else {
                return Ok(None);
            };
            if let Some(frame) = self.handle_public_process_event_envelope_nowait(envelope)? {
                return Ok(Some(frame));
            }
        }
    }

    /// Borrowed-host compatibility path for process-targeted polling. The stdio
    /// extension path uses the broker-backed owned service instead, so its
    /// timeout never retains the sidecar coordinator.
    pub async fn poll_process_event(
        &mut self,
        ownership: &OwnershipScope,
        process_id: &str,
        timeout: Duration,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let target = ProcessEventTarget::for_owned_process(ownership, process_id)
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        let (connection_id, session_id, vm_id) = self.vm_scope_for(ownership)?;
        self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
        let deadline = Instant::now() + timeout;
        let process_event_notify = Arc::clone(&self.process_event_notify);
        loop {
            let notified = process_event_notify.notified();
            self.pump_process_events(ownership).await?;
            while let Some(envelope) =
                self.take_matching_process_event_envelope(&target.vm_id, &target.process_id)?
            {
                if let Some(frame) = self.handle_process_event_envelope(envelope).await? {
                    return Ok(Some(frame));
                }
            }
            if timeout.is_zero() || Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::select! {
                _ = notified => {}
                _ = time::sleep(deadline.saturating_duration_since(Instant::now())) => {
                    return Ok(None);
                }
            }
        }
    }

    pub(crate) async fn handle_process_event_envelope(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let handle_start = Instant::now();
        let ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id,
            child_path,
            process_id,
            event,
        } = envelope;

        let is_exit_event = matches!(event, ActiveExecutionEvent::Exited(_));

        if is_exit_event {
            record_execute_exit_event_queue_wait(
                "process_exit_event_queue_wait",
                &vm_id,
                &process_id,
            );
            let mut trailing = Vec::new();
            let mut deferred = VecDeque::new();
            let phase_start = Instant::now();
            while let Some(pending) = self.pending_process_events.pop_front() {
                if pending.vm_id == vm_id
                    && pending.process_id == process_id
                    && !matches!(pending.event, ActiveExecutionEvent::Exited(_))
                {
                    trailing.push(pending.event);
                } else {
                    deferred.push_back(pending);
                }
            }
            self.pending_process_events = deferred;
            self.observe_pending_process_event_depth();
            self.rearm_deferred_process_event_after_capacity_release();
            record_execute_phase("process_exit_trailing_pending_scan", phase_start.elapsed());
            if !trailing.is_empty() {
                if self.pending_process_event_capacity() < trailing.len() {
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                let emit_now = if self.pending_process_event_capacity() == trailing.len() {
                    Some(trailing.remove(0))
                } else {
                    None
                };
                let phase_start = Instant::now();
                mark_execute_exit_event_queued(&vm_id, &process_id);
                self.queue_front_pending_process_event(ProcessEventEnvelope {
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    child_path: child_path.clone(),
                    process_id: process_id.clone(),
                    event,
                })?;
                for event in trailing.into_iter().rev() {
                    self.queue_front_pending_process_event(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
                        child_path: child_path.clone(),
                        process_id: process_id.clone(),
                        event,
                    })?;
                }
                record_execute_phase("process_exit_trailing_requeue", phase_start.elapsed());
                if let Some(event) = emit_now {
                    let result = self
                        .handle_execution_event(&vm_id, &process_id, event)
                        .await;
                    record_execute_phase(
                        "process_exit_event_handle_envelope_total",
                        handle_start.elapsed(),
                    );
                    return result;
                }
                record_execute_phase(
                    "process_exit_event_handle_envelope_total",
                    handle_start.elapsed(),
                );
                return Ok(None);
            }
        }

        let result = self
            .handle_execution_event(&vm_id, &process_id, event)
            .await;
        if is_exit_event {
            let target = ProcessEventTarget::new(
                connection_id,
                session_id,
                vm_id.clone(),
                process_id.clone(),
            )
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
            if let Err(error) = self
                .process_event_broker
                .dispose_process(&target, OperationCancellationReason::Explicit)
            {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_EVENT_PROCESS_DISPOSAL: vm_id={vm_id} process_id={process_id} error={error}"
                );
            }
            record_execute_phase(
                "process_exit_event_handle_envelope_total",
                handle_start.elapsed(),
            );
        }
        result
    }

    /// Synchronous completion-side form used by the detached extension/event
    /// supervisors. Broker envelopes contain only public events; internal RPC
    /// work is serviced before publication.
    pub(crate) fn handle_public_process_event_envelope_nowait(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id,
            child_path,
            process_id,
            event,
        } = envelope;
        let is_exit_event = matches!(event, ActiveExecutionEvent::Exited(_));

        if is_exit_event {
            record_execute_exit_event_queue_wait(
                "process_exit_event_queue_wait",
                &vm_id,
                &process_id,
            );
            let mut trailing = Vec::new();
            let mut deferred = VecDeque::new();
            while let Some(pending) = self.pending_process_events.pop_front() {
                if pending.vm_id == vm_id
                    && pending.process_id == process_id
                    && !matches!(pending.event, ActiveExecutionEvent::Exited(_))
                {
                    trailing.push(pending.event);
                } else {
                    deferred.push_back(pending);
                }
            }
            self.pending_process_events = deferred;
            self.observe_pending_process_event_depth();
            self.rearm_deferred_process_event_after_capacity_release();
            if !trailing.is_empty() {
                if self.pending_process_event_capacity() < trailing.len() {
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                let emit_now = if self.pending_process_event_capacity() == trailing.len() {
                    Some(trailing.remove(0))
                } else {
                    None
                };
                mark_execute_exit_event_queued(&vm_id, &process_id);
                self.queue_front_pending_process_event(ProcessEventEnvelope {
                    connection_id: connection_id.clone(),
                    session_id: session_id.clone(),
                    vm_id: vm_id.clone(),
                    child_path: child_path.clone(),
                    process_id: process_id.clone(),
                    event,
                })?;
                for event in trailing.into_iter().rev() {
                    self.queue_front_pending_process_event(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
                        child_path: child_path.clone(),
                        process_id: process_id.clone(),
                        event,
                    })?;
                }
                return emit_now.map_or(Ok(None), |event| {
                    self.handle_public_execution_event_nowait(&vm_id, &process_id, event)
                });
            }
        }

        let result = self.handle_public_execution_event_nowait(&vm_id, &process_id, event);
        if is_exit_event {
            let target = ProcessEventTarget::new(
                connection_id,
                session_id,
                vm_id.clone(),
                process_id.clone(),
            )
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
            if let Err(error) = self
                .process_event_broker
                .dispose_process(&target, OperationCancellationReason::Explicit)
            {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_EVENT_PROCESS_DISPOSAL: vm_id={vm_id} process_id={process_id} error={error}"
                );
            }
        }
        result
    }

    // try_poll_event moved to crate::execution

    pub async fn close_session(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.dispose_session(connection_id, session_id, DisposeReason::Requested)
            .await
    }

    pub async fn remove_connection(
        &mut self,
        connection_id: &str,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_authenticated_connection(connection_id)?;

        let session_ids = self
            .connections
            .get(connection_id)
            .expect("authenticated connection should exist")
            .sessions
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut events = Vec::new();
        let mut first_error: Option<SidecarError> = None;
        for session_id in session_ids {
            // Attempt EVERY session; aggregate errors instead of `?`-ing out on
            // the first so one wedged session cannot abandon the rest (H1).
            match self
                .dispose_session(connection_id, &session_id, DisposeReason::ConnectionClosed)
                .await
            {
                Ok(session_events) => events.extend(session_events),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        if let Err(error) = self
            .process_event_broker
            .dispose_connection(connection_id, OperationCancellationReason::ConnectionClosed)
        {
            eprintln!(
                "ERR_AGENTOS_PROCESS_EVENT_CONNECTION_DISPOSAL: connection_id={connection_id} error={error}"
            );
        }

        self.connections.remove(connection_id);
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(events)
    }

    /// Apply the bounded central-state portion of a prepared membership request.
    ///
    /// The stdio router calls this immediately after preparation so pipelined
    /// dependent frames observe membership without waiting for terminal output.
    /// `complete_request` calls it again for in-process users, so the operation
    /// is deliberately idempotent.
    pub(crate) fn commit_prepared_membership(
        &mut self,
        membership: &PreparedMembershipCommit,
    ) -> Result<(), SidecarError> {
        match membership {
            PreparedMembershipCommit::Connection {
                connection_id,
                auth_token,
            } => {
                if let Some(existing) = self.connections.get(connection_id) {
                    if existing.auth_token != *auth_token {
                        return Err(SidecarError::InvalidState(format!(
                            "ERR_AGENTOS_CONNECTION_COMMIT_CONFLICT: {connection_id} already has different authentication state"
                        )));
                    }
                    return Ok(());
                }
                let expected = format!("conn-{}", self.next_connection_id.saturating_add(1));
                if connection_id != &expected {
                    return Err(SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_CONNECTION_COMMIT_ORDER: prepared {connection_id}, expected {expected}"
                    )));
                }
                self.next_connection_id = self.next_connection_id.saturating_add(1);
                self.connections.insert(
                    connection_id.clone(),
                    ConnectionState {
                        auth_token: auth_token.clone(),
                        sessions: BTreeSet::new(),
                    },
                );
            }
            PreparedMembershipCommit::Session {
                connection_id,
                session_id,
                placement,
                metadata,
            } => {
                self.require_authenticated_connection(connection_id)?;
                if let Some(existing) = self.sessions.get(session_id) {
                    if existing.connection_id != *connection_id {
                        return Err(SidecarError::InvalidState(format!(
                            "ERR_AGENTOS_SESSION_COMMIT_CONFLICT: {session_id} is already owned by {}",
                            existing.connection_id
                        )));
                    }
                    return Ok(());
                }
                let expected = format!("session-{}", self.next_session_id.saturating_add(1));
                if session_id != &expected {
                    return Err(SidecarError::InvalidState(format!(
                        "ERR_AGENTOS_SESSION_COMMIT_ORDER: prepared {session_id}, expected {expected}"
                    )));
                }
                self.next_session_id = self.next_session_id.saturating_add(1);
                self.sessions.insert(
                    session_id.clone(),
                    SessionState {
                        connection_id: connection_id.clone(),
                        placement: placement.clone(),
                        metadata: metadata.clone(),
                        vm_ids: BTreeSet::new(),
                    },
                );
                self.connections
                    .get_mut(connection_id)
                    .expect("authenticated connection checked before session commit")
                    .sessions
                    .insert(session_id.clone());
            }
        }
        Ok(())
    }

    fn authenticate_connection(
        &mut self,
        request: &RequestFrame,
        payload: crate::protocol::AuthenticateRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let _ = self.connection_id_for(&request.ownership)?;
        if let Err(error) = self.validate_auth_token(&payload.auth_token) {
            let mut fields = audit_fields([
                (String::from("source"), payload.client_name.clone()),
                (String::from("reason"), error.to_string()),
            ]);
            if let OwnershipScope::ConnectionOwnership(inner) = &request.ownership {
                fields.insert(String::from("connection_id"), inner.connection_id.clone());
            }
            emit_security_audit_event(
                &self.bridge,
                &self.config.sidecar_id,
                "security.auth.failed",
                fields,
            );
            return Err(error);
        }

        if let Err(error) = validate_authenticate_versions(&payload) {
            return Err(match error {
                AuthenticateVersionError::ProtocolVersionMismatch(message) => {
                    SidecarError::ProtocolVersionMismatch(message)
                }
                AuthenticateVersionError::BridgeVersionMismatch(message) => {
                    SidecarError::BridgeVersionMismatch(message)
                }
            });
        }

        let connection_id = self.allocate_connection_id();
        self.connections.insert(
            connection_id.clone(),
            ConnectionState {
                auth_token: payload.auth_token,
                sessions: BTreeSet::new(),
            },
        );

        let response = shared_authenticated_response(
            request.request_id,
            self.config.sidecar_id.clone(),
            connection_id,
            self.config.max_frame_bytes as u32,
        );
        Ok(DispatchResult {
            response,
            events: Vec::new(),
        })
    }

    fn open_session(
        &mut self,
        request: &RequestFrame,
        payload: OpenSessionRequest,
    ) -> Result<DispatchResult, SidecarError> {
        let connection_id = self.connection_id_for(&request.ownership)?;
        self.require_authenticated_connection(&connection_id)?;

        self.next_session_id += 1;
        let session_id = format!("session-{}", self.next_session_id);
        self.sessions.insert(
            session_id.clone(),
            SessionState {
                connection_id: connection_id.clone(),
                placement: payload.placement,
                metadata: payload.metadata.into_iter().collect(),
                vm_ids: BTreeSet::new(),
            },
        );
        self.connections
            .get_mut(&connection_id)
            .expect("authenticated connection should exist")
            .sessions
            .insert(session_id.clone());

        Ok(DispatchResult {
            response: session_opened_response(request.request_id, connection_id, session_id),
            events: Vec::new(),
        })
    }

    // create_vm, dispose_vm, bootstrap_root_filesystem, configure_vm moved to crate::vm

    async fn guest_filesystem_call(
        &mut self,
        request: &RequestFrame,
        payload: GuestFilesystemCallRequest,
    ) -> Result<DispatchResult, SidecarError> {
        filesystem_guest_filesystem_call(self, request, payload).await
    }

    // snapshot_root_filesystem moved to crate::vm

    // execute, write_stdin, close_stdin, kill_process, find_listener, find_bound_udp,
    // get_signal_state, get_zombie_timer_count moved to crate::execution

    async fn dispose_session(
        &mut self,
        connection_id: &str,
        session_id: &str,
        reason: DisposeReason,
    ) -> Result<Vec<EventFrame>, SidecarError> {
        self.require_owned_session(connection_id, session_id)?;

        let cancellation_reason = match &reason {
            DisposeReason::Requested => OperationCancellationReason::Explicit,
            DisposeReason::ConnectionClosed => OperationCancellationReason::ConnectionClosed,
            DisposeReason::HostShutdown => OperationCancellationReason::Shutdown,
        };
        if let Err(error) = self.process_event_broker.dispose_session(
            connection_id,
            session_id,
            cancellation_reason,
        ) {
            eprintln!(
                "ERR_AGENTOS_PROCESS_EVENT_SESSION_DISPOSAL: connection_id={connection_id} session_id={session_id} error={error}"
            );
        }

        let vm_ids = self
            .sessions
            .get(session_id)
            .expect("owned session should exist")
            .vm_ids
            .iter()
            .cloned()
            .collect::<Vec<_>>();

        let mut events = Vec::new();
        let mut first_error: Option<SidecarError> = None;
        for vm_id in vm_ids {
            // Attempt EVERY VM; aggregate errors instead of `?`-ing out on the
            // first so one stuck VM cannot strand the remaining VMs' teardown and
            // leave the session permanently un-reclaimed (H1).
            match self
                .dispose_vm_internal(connection_id, session_id, &vm_id, reason.clone())
                .await
            {
                Ok(vm_events) => events.extend(vm_events),
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        // On client disconnect, give every registered extension a chance to free
        // the per-session state it tracks (H4): the host owns the only signal an
        // extension gets that a session has gone away.
        if matches!(reason, DisposeReason::ConnectionClosed) {
            if let Err(error) = self
                .dispose_extension_session_state(connection_id, session_id)
                .await
            {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }

        self.sessions.remove(session_id);
        if let Some(connection) = self.connections.get_mut(connection_id) {
            connection.sessions.remove(session_id);
        }
        // Tell the stdio transport this session is gone so it stops iterating a
        // dead entry every event-pump tick and the set stops growing (M5).
        self.disposed_sessions
            .push((connection_id.to_owned(), session_id.to_owned()));

        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(events)
    }

    /// Invoke each registered extension's per-session teardown hook so it can
    /// release the state it keyed on this host session. Errors are aggregated so
    /// one misbehaving extension cannot prevent the others from cleaning up.
    async fn dispose_extension_session_state(
        &mut self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<(), SidecarError> {
        let ownership = OwnershipScope::session(connection_id, session_id);
        let extensions = self
            .extensions
            .values()
            .cloned()
            .collect::<Vec<Arc<dyn Extension>>>();
        let mut first_error: Option<SidecarError> = None;
        for extension in extensions {
            let snapshot = ExtensionSnapshot::new(
                extension.namespace().to_owned(),
                ownership.clone(),
                self.sidecar_requests.clone(),
                self.event_sink.clone(),
            );
            if let Err(error) = extension.on_session_disposed(snapshot).await {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Drain the session scopes disposed since the last call so the stdio
    /// transport can untrack them from its active-session set (M5).
    pub(crate) fn take_disposed_sessions(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.disposed_sessions)
    }

    // dispose_vm_internal, terminate_vm_processes, wait_for_vm_processes_to_exit moved to crate::vm

    // kill_process_internal, handle_execution_event, handle_python_vfs_rpc_request,
    // resolve_javascript_child_process_execution, spawn_javascript_child_process,
    // poll_javascript_child_process, write_javascript_child_process_stdin,
    // close_javascript_child_process_stdin, kill_javascript_child_process moved to crate::execution

    /// Whether a `__kernel_stdin_read` / `__kernel_poll` RPC may be serviced
    /// via the non-blocking deferral path. Non-TTY JavaScript keeps its
    /// in-process local stdin bridge (serviced inline by the fallback arm).
    fn kernel_wait_rpc_is_deferrable(
        &self,
        vm_id: &str,
        process_id: &str,
        request: &JavascriptSyncRpcRequest,
    ) -> bool {
        let Some(vm) = self.vms.get(vm_id) else {
            return false;
        };
        let Some(process) = vm.active_processes.get(process_id) else {
            return false;
        };
        if request.method == "__kernel_stdin_read"
            && matches!(
                process.execution,
                crate::state::ActiveExecution::Javascript(_)
            )
            && process.tty_master_fd.is_none()
        {
            return false;
        }
        true
    }

    /// Service `__kernel_stdin_read` / `__kernel_poll` without blocking the
    /// dispatch loop. Probes readiness with a zero timeout; when not ready and
    /// the requested timeout has not expired, parks the RPC on the process
    /// (reply-by-token) and spawns a waiter that re-enqueues it as a process
    /// event when kernel poll state changes or the deadline passes. The kernel
    /// waits stay event-driven (PollNotifier), so a host stdin write wakes the
    /// guest immediately instead of after a polling slice.
    ///
    /// Returns `Ok(Some(response))` to reply now, `Ok(None)` when parked.
    fn service_deferrable_kernel_wait_rpc(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: &JavascriptSyncRpcRequest,
    ) -> Result<Option<crate::execution::JavascriptSyncRpcServiceResponse>, SidecarError> {
        let requested_timeout_ms = match request.method.as_str() {
            "process.fd_write" => None,
            "__kernel_stdin_read" => parse_kernel_stdin_read_args(request)?.1,
            _ => {
                let timeout_ms = parse_kernel_poll_args(request)?.1;
                (timeout_ms >= 0).then_some(timeout_ms as u64)
            }
        };
        let now = Instant::now();

        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "deferred kernel wait RPC");
            return Ok(None);
        };
        let wait_handle = vm.kernel.poll_wait_handle();
        // Snapshot BEFORE the readiness probe: a write landing between the
        // probe and the waiter's wait bumps the generation, so the wait
        // returns immediately instead of losing the wakeup.
        let generation = wait_handle.snapshot();
        let requested_timeout_ms = if request.method == "process.fd_write" {
            Some(vm.limits.reactor.operation_deadline_ms)
        } else {
            requested_timeout_ms
        };
        let VmState {
            kernel,
            active_processes,
            ..
        } = &mut *vm;
        let Some(process) = active_processes.get_mut(process_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "deferred kernel wait RPC");
            return Ok(None);
        };
        // Reading from the pipe frees capacity. Top it off before every root
        // process read/poll probe, matching the descendant-process path, and
        // deliver a deferred close only after all accepted bytes are written.
        flush_pending_kernel_stdin(kernel, process)?;
        let kernel_pid = process.kernel_pid;
        let kernel_stdin_reader_fd = process.kernel_stdin_reader_fd;
        let deadline = match &process.deferred_kernel_wait_rpc {
            Some((parked, parked_deadline)) if parked.id == request.id => *parked_deadline,
            _ => requested_timeout_ms.map(|timeout_ms| now + Duration::from_millis(timeout_ms)),
        };
        let probe = match request.method.as_str() {
            "process.fd_write" => {
                service_javascript_kernel_fd_write_sync_rpc(kernel, process, request)
            }
            "__kernel_stdin_read" => {
                let (max_bytes, _) = parse_kernel_stdin_read_args(request)?;
                kernel_stdin_read_response(
                    kernel,
                    kernel_pid,
                    kernel_stdin_reader_fd,
                    max_bytes,
                    Duration::ZERO,
                )
            }
            _ => {
                let (fd_requests, _) = parse_kernel_poll_args(request)?;
                kernel_poll_response(&vm.kernel, kernel_pid, &fd_requests, 0)
            }
        };
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(None);
        };
        let (probe, ready) = match probe {
            Ok(value) => {
                let ready = match request.method.as_str() {
                    "process.fd_write" => true,
                    "__kernel_stdin_read" => !value.is_null(),
                    _ => value.get("readyCount").and_then(Value::as_u64).unwrap_or(0) > 0,
                };
                (value, ready)
            }
            Err(error)
                if request.method == "process.fd_write"
                    && javascript_sync_rpc_error_code(&error) == "EAGAIN" =>
            {
                (Value::Null, false)
            }
            Err(error) => {
                process.deferred_kernel_wait_rpc = None;
                return Err(error);
            }
        };
        if request.method == "process.fd_write"
            && !ready
            && deadline.is_some_and(|deadline| now >= deadline)
        {
            process.deferred_kernel_wait_rpc = None;
            return Err(SidecarError::Execution(format!(
                "ETIMEDOUT: pipe write exceeded limits.reactor.operationDeadlineMs ({} ms); raise that limit for slower readers",
                vm.limits.reactor.operation_deadline_ms
            )));
        }
        if ready
            || requested_timeout_ms == Some(0)
            || deadline.is_some_and(|deadline| now >= deadline)
        {
            process.deferred_kernel_wait_rpc = None;
            return Ok(Some(probe.into()));
        }

        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let runtime = vm.runtime_context.clone();
        let remaining = deadline.map(|deadline| deadline.saturating_duration_since(now));
        let sender = self.process_event_sender.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        let waiter_request = request.clone();
        let envelope_vm_id = vm_id.to_owned();
        let envelope_process_id = process_id.to_owned();
        runtime
            .spawn(agentos_runtime::TaskClass::Vm, async move {
            // Wake on any kernel poll-state change or the deadline; either way
            // requeue exactly once. The handler re-probes and either replies or
            // re-parks without dedicating an OS thread to this wait.
            if let Some(remaining) = remaining {
                tokio::select! {
                    _ = wait_handle.wait_for_change_async(generation) => {}
                    _ = tokio::time::sleep(remaining) => {}
                }
            } else {
                wait_handle.wait_for_change_async(generation).await;
            }
            if sender
                .send(ProcessEventEnvelope {
                    connection_id,
                    session_id,
                    vm_id: envelope_vm_id,
                    child_path: Vec::new(),
                    process_id: envelope_process_id,
                    event: ActiveExecutionEvent::JavascriptSyncRpcRequest(waiter_request),
                })
                .await
                .is_err()
            {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: deferred kernel wait completion could not be delivered"
                );
            } else {
                event_notify.notify_one();
            }
            })
            .map_err(SidecarError::from)?;
        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            return Ok(None);
        };
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(None);
        };
        process.deferred_kernel_wait_rpc = Some((request.clone(), deadline));
        Ok(None)
    }

    pub(crate) async fn handle_javascript_sync_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        request: JavascriptSyncRpcRequest,
    ) -> Result<(), SidecarError> {
        record_sync_bridge_request_observed(request.id, &request.method);
        let Some(process_active) = self
            .vms
            .get(vm_id)
            .map(|vm| vm.active_processes.contains_key(process_id))
        else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "javascript sync RPC");
            return Ok(());
        };
        if !process_active {
            log_stale_process_event(&self.bridge, vm_id, process_id, "javascript sync RPC");
            return Ok(());
        }

        let deferrable_fd_write = {
            let vm = self.vms.get(vm_id).expect("VM existence checked above");
            let process = vm
                .active_processes
                .get(process_id)
                .expect("process existence checked above");
            deferred_kernel_wait_request_for_process(&request, &vm.kernel, process)?
                .filter(|request| request.method == "process.fd_write")
        };

        let response: Result<crate::execution::JavascriptSyncRpcServiceResponse, SidecarError> =
            match request.method.as_str() {
                _ if deferrable_fd_write.is_some() => {
                    let normalized = deferrable_fd_write
                        .as_ref()
                        .expect("guarded deferred fd_write request");
                    match self.service_deferrable_kernel_wait_rpc(vm_id, process_id, normalized) {
                        Ok(Some(response)) => Ok(response),
                        Ok(None) => return Ok(()),
                        Err(error) => Err(error),
                    }
                }
                "child_process.spawn" => {
                    let payload = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC child_process.spawn",
                            );
                            return Ok(());
                        };
                        parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                    };
                    self.spawn_descendant_javascript_child_process(vm_id, process_id, &[], payload)
                        .await
                        .map(Into::into)
                }
                "child_process.spawn_sync" => {
                    let (payload, max_buffer) = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC child_process.spawn_sync",
                            );
                            return Ok(());
                        };
                        parse_javascript_child_process_spawn_request(&vm, &request.args)?
                    };
                    self.defer_javascript_child_process_sync(vm_id, process_id, payload, max_buffer)
                        .await
                }
                "child_process.poll" => {
                    let child_process_id = javascript_sync_rpc_arg_str(
                        &request.args,
                        0,
                        "child_process.poll child id",
                    )?;
                    let wait_ms = javascript_sync_rpc_arg_u64_optional(
                        &request.args,
                        1,
                        "child_process.poll wait ms",
                    )?
                    .unwrap_or_default();
                    self.poll_javascript_child_process(vm_id, process_id, child_process_id, wait_ms)
                        .await
                        .map(Into::into)
                }
                "child_process.write_stdin" => {
                    let child_process_id = javascript_sync_rpc_arg_str(
                        &request.args,
                        0,
                        "child_process.write_stdin child id",
                    )?;
                    let chunk = javascript_sync_rpc_bytes_arg(
                        &request.args,
                        1,
                        "child_process.write_stdin chunk",
                    )?;
                    self.write_javascript_child_process_stdin(
                        vm_id,
                        process_id,
                        child_process_id,
                        &chunk,
                    )
                    .map(|()| Value::Null.into())
                }
                "child_process.close_stdin" => {
                    let child_process_id = javascript_sync_rpc_arg_str(
                        &request.args,
                        0,
                        "child_process.close_stdin child id",
                    )?;
                    self.close_javascript_child_process_stdin(vm_id, process_id, child_process_id)
                        .map(|()| Value::Null.into())
                }
                "child_process.kill" => {
                    let child_process_id = javascript_sync_rpc_arg_str(
                        &request.args,
                        0,
                        "child_process.kill child id",
                    )?;
                    let signal =
                        javascript_sync_rpc_arg_str(&request.args, 1, "child_process.kill signal")?;
                    self.kill_javascript_child_process(vm_id, process_id, child_process_id, signal)
                        .map(|()| Value::Null.into())
                }
                "process.exec_fd_image_commit" => {
                    let payload = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC process.exec_fd_image_commit",
                            );
                            return Ok(());
                        };
                        parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                    };
                    self.commit_wasm_fd_process_image(vm_id, process_id, &[], payload)?;
                    Ok(json!({ "committed": true }).into())
                }
                "process.exec" => {
                    let payload = {
                        let Some(vm) = self.vms.get(vm_id) else {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC process.exec",
                            );
                            return Ok(());
                        };
                        parse_javascript_child_process_spawn_request(&vm, &request.args)?.0
                    };
                    let local_replacement = payload.options.local_replacement;
                    match self.exec_javascript_process_image(vm_id, process_id, &[], payload) {
                        Ok(()) if local_replacement => Ok(json!({ "committed": true }).into()),
                        // Success destroys the blocked old image. Never reply:
                        // returning would resume instructions after execve.
                        Ok(()) => return Ok(()),
                        Err(error) => Err(error),
                    }
                }
                "process.kill" => {
                    let target_pid =
                        javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
                    let signal =
                        javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
                    let parsed_signal = parse_signal(signal)?;
                    if parsed_signal == 0 {
                        let Some(vm) = self.vms.get(vm_id) else {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC process.kill",
                            );
                            return Ok(());
                        };
                        if !vm.active_processes.contains_key(process_id) {
                            log_stale_process_event(
                                &self.bridge,
                                vm_id,
                                process_id,
                                "javascript sync RPC process.kill",
                            );
                            return Ok(());
                        }
                        vm.kernel
                            .signal_process(EXECUTION_DRIVER_NAME, target_pid, parsed_signal)
                            .map(|()| Value::Null.into())
                            .map_err(kernel_error)
                    } else if target_pid < 0 {
                        let caller_kernel_pid = {
                            let Some(vm) = self.vms.get(vm_id) else {
                                log_stale_process_event(
                                    &self.bridge,
                                    vm_id,
                                    process_id,
                                    "javascript sync RPC process.kill",
                                );
                                return Ok(());
                            };
                            let Some(caller) = vm.active_processes.get(process_id) else {
                                log_stale_process_event(
                                    &self.bridge,
                                    vm_id,
                                    process_id,
                                    "javascript sync RPC process.kill",
                                );
                                return Ok(());
                            };
                            caller.kernel_pid
                        };
                        let pgid = target_pid.unsigned_abs();
                        match self.signal_vm_process_group(vm_id, caller_kernel_pid, pgid, signal) {
                            Ok(true) => self
                                .apply_self_process_kill(vm_id, process_id, parsed_signal)
                                .map(Into::into),
                            Ok(false) => Ok(Value::Null.into()),
                            Err(error) => Err(error),
                        }
                    } else {
                        enum ProcessKillTarget {
                            SelfProcess,
                            Child(String),
                            TopLevel(String),
                            KernelPid(u32),
                        }
                        let target = {
                            let Some(vm) = self.vms.get(vm_id) else {
                                log_stale_process_event(
                                    &self.bridge,
                                    vm_id,
                                    process_id,
                                    "javascript sync RPC process.kill",
                                );
                                return Ok(());
                            };
                            let Some(caller) = vm.active_processes.get(process_id) else {
                                log_stale_process_event(
                                    &self.bridge,
                                    vm_id,
                                    process_id,
                                    "javascript sync RPC process.kill",
                                );
                                return Ok(());
                            };
                            let caller_pid = i32::try_from(caller.kernel_pid).map_err(|_| {
                                SidecarError::InvalidState("caller pid exceeds i32".into())
                            })?;
                            if caller_pid == target_pid {
                                ProcessKillTarget::SelfProcess
                            } else if let Some((child_process_id, _)) =
                                caller.child_processes.iter().find(|(_, child)| {
                                    i32::try_from(child.kernel_pid) == Ok(target_pid)
                                })
                            {
                                ProcessKillTarget::Child(child_process_id.clone())
                            } else if let Some((target_process_id, _)) =
                                vm.active_processes.iter().find(|(_, process)| {
                                    i32::try_from(process.kernel_pid) == Ok(target_pid)
                                })
                            {
                                ProcessKillTarget::TopLevel(target_process_id.clone())
                            } else {
                                let target_kernel_pid =
                                    u32::try_from(target_pid).map_err(|_| {
                                        SidecarError::InvalidState(format!(
                                            "EINVAL: invalid process pid {target_pid}"
                                        ))
                                    })?;
                                ProcessKillTarget::KernelPid(target_kernel_pid)
                            }
                        };
                        match target {
                            ProcessKillTarget::SelfProcess => self
                                .apply_self_process_kill(vm_id, process_id, parsed_signal)
                                .map(Into::into),
                            ProcessKillTarget::Child(child_process_id) => {
                                self.kill_javascript_child_process(
                                    vm_id,
                                    process_id,
                                    &child_process_id,
                                    signal,
                                )?;
                                Ok(Value::Null.into())
                            }
                            ProcessKillTarget::TopLevel(target_process_id) => {
                                self.kill_process_internal(vm_id, &target_process_id, signal)?;
                                Ok(Value::Null.into())
                            }
                            ProcessKillTarget::KernelPid(target_kernel_pid) => {
                                // Grandchildren and untracked kernel processes are
                                // resolved VM-wide instead of failing with an
                                // unknown-pid error.
                                self.signal_vm_kernel_pid(vm_id, target_kernel_pid, signal)
                                    .map(|()| Value::Null.into())
                            }
                        }
                    }
                }
                "process.signal_state" => {
                    let (signal, registration) = parse_process_signal_state_request(&request.args)
                        .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        log_stale_process_event(
                            &self.bridge,
                            vm_id,
                            process_id,
                            "javascript sync RPC process.signal_state",
                        );
                        return Ok(());
                    };
                    apply_process_signal_state_update(
                        &mut vm.signal_states,
                        process_id,
                        signal,
                        registration,
                    );
                    Ok(Value::Null.into())
                }
                "net.http_request" => {
                    let payload = request
                        .args
                        .first()
                        .cloned()
                        .ok_or_else(|| {
                            SidecarError::InvalidState(String::from(
                                "net.http_request requires a request payload",
                            ))
                        })
                        .and_then(|value| {
                            serde_json::from_value::<JavascriptHttpLoopbackRequest>(value).map_err(
                                |error| {
                                    SidecarError::InvalidState(format!(
                                        "invalid net.http_request payload: {error}"
                                    ))
                                },
                            )
                        })?;
                    if !is_javascript_loopback_host(&payload.host) {
                        return Err(SidecarError::Execution(format!(
                            "EACCES: HTTP loopback request requires a loopback host, got {}",
                            payload.host
                        )));
                    }
                    self.bridge.require_network_access(
                        vm_id,
                        NetworkOperation::Http,
                        format_tcp_resource(&payload.host, payload.port),
                    )?;
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        log_stale_process_event(
                            &self.bridge,
                            vm_id,
                            process_id,
                            "javascript sync RPC net.http_request",
                        );
                        return Ok(());
                    };
                    let socket_paths = build_javascript_socket_path_context(&mut vm)?;
                    let target_is_current =
                        [JavascriptSocketFamily::Ipv4, JavascriptSocketFamily::Ipv6]
                            .iter()
                            .any(|family| {
                                socket_paths
                                    .http_loopback_target(*family, payload.port)
                                    .is_some_and(|target| {
                                        target.process_id == payload.process_id
                                            && target.server_id == payload.server_id
                                    })
                            });
                    if !target_is_current {
                        return Err(SidecarError::InvalidState(format!(
                            "unknown HTTP loopback target {}:{} for server {} in process {}",
                            payload.host, payload.port, payload.server_id, payload.process_id
                        )));
                    }
                    let VmState {
                        dns,
                        kernel,
                        kernel_socket_readiness,
                        capabilities,
                        active_processes,
                        ..
                    } = &mut *vm;
                    let kernel_readiness = Arc::clone(kernel_socket_readiness);
                    let capabilities = capabilities.clone();
                    let Some(target_process) = active_processes.get_mut(&payload.process_id) else {
                        return Err(SidecarError::InvalidState(format!(
                            "unknown HTTP loopback process {}",
                            payload.process_id
                        )));
                    };
                    dispatch_loopback_http_request_deferred(LoopbackHttpDispatchRequest {
                        bridge: &self.bridge,
                        vm_id,
                        dns,
                        socket_paths: &socket_paths,
                        kernel,
                        kernel_readiness,
                        process: target_process,
                        server_id: payload.server_id,
                        request_json: &payload.request,
                        capabilities,
                    })
                }
                "__kernel_stdio_write"
                    if self.vms.get(vm_id).is_some_and(|vm| {
                        vm.active_processes
                            .get(process_id)
                            .is_some_and(|process| process.tty_master_owner.is_some())
                    }) =>
                {
                    let (writer_kernel_pid, owner) = {
                        self.vms
                            .get(vm_id)
                            .and_then(|vm| {
                                vm.active_processes.get(process_id).map(|process| {
                                    (
                                        process.kernel_pid,
                                        process.tty_master_owner.expect("guarded by match arm"),
                                    )
                                })
                            })
                            .expect("guarded by match arm")
                    };
                    self.service_shared_tty_stdio_write(vm_id, writer_kernel_pid, owner, &request)
                        .map(Into::into)
                }
                "__kernel_stdin_read" | "__kernel_poll"
                    if self.kernel_wait_rpc_is_deferrable(vm_id, process_id, &request) =>
                {
                    match self.service_deferrable_kernel_wait_rpc(vm_id, process_id, &request) {
                        Ok(Some(response)) => Ok(response),
                        // Parked: an off-loop waiter re-enqueues this request as a
                        // process event when kernel poll state changes.
                        Ok(None) => return Ok(()),
                        Err(error) => Err(error),
                    }
                }
                _ => {
                    let Some(mut vm) = self.vms.get_mut(vm_id) else {
                        log_stale_process_event(
                            &self.bridge,
                            vm_id,
                            process_id,
                            "javascript sync RPC bridge dispatch",
                        );
                        return Ok(());
                    };
                    let socket_paths = build_javascript_socket_path_context(&mut vm)?;
                    let VmState {
                        dns,
                        kernel,
                        kernel_socket_readiness,
                        capabilities,
                        active_processes,
                        ..
                    } = &mut *vm;
                    let kernel_readiness = Arc::clone(kernel_socket_readiness);
                    let capabilities = capabilities.clone();
                    let Some(process) = active_processes.get_mut(process_id) else {
                        log_stale_process_event(
                            &self.bridge,
                            vm_id,
                            process_id,
                            "javascript sync RPC bridge dispatch",
                        );
                        return Ok(());
                    };
                    service_javascript_sync_rpc(JavascriptSyncRpcServiceRequest {
                        bridge: &self.bridge,
                        vm_id,
                        dns,
                        socket_paths: &socket_paths,
                        kernel,
                        kernel_readiness,
                        process,
                        sync_request: &request,
                        capabilities,
                    })
                    .await
                }
            };

        let response = match response {
            Ok(crate::execution::JavascriptSyncRpcServiceResponse::Deferred {
                receiver,
                timeout,
                task_class,
            }) => {
                let Some(vm) = self.vms.get(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "deferred sync RPC response admission",
                    );
                    return Ok(());
                };
                let runtime = vm.runtime_context.clone();
                let connection_id = vm.connection_id.clone();
                let session_id = vm.session_id.clone();
                let sender = self.process_event_sender.clone();
                let event_notify = Arc::clone(&self.process_event_notify);
                let envelope_vm_id = vm_id.to_owned();
                let envelope_process_id = process_id.to_owned();
                let request_id = request.id;
                let method = request.method.clone();
                runtime
                .spawn(task_class, async move {
                    let receive = async {
                        receiver.await.unwrap_or_else(|_| {
                            Err(crate::state::DeferredRpcError {
                                code: String::from(
                                    "ERR_AGENTOS_DEFERRED_RPC_RESPONSE_CHANNEL_CLOSED",
                                ),
                                message: format!(
                                    "deferred sync RPC response channel closed for {method}"
                                ),
                            })
                        })
                    };
                    let result = match timeout {
                        Some(timeout) => match tokio::time::timeout(timeout, receive).await {
                            Ok(result) => result,
                            Err(_) => Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_AGENTOS_DEFERRED_RPC_TIMEOUT"),
                                message: format!(
                                    "{method} exceeded limits.reactor.operationDeadlineMs ({} ms); raise that limit for slower peers",
                                    timeout.as_millis()
                                ),
                            }),
                        },
                        None => receive.await,
                    };
                    if sender
                        .send(ProcessEventEnvelope {
                            connection_id,
                            session_id,
                            vm_id: envelope_vm_id,
                            child_path: Vec::new(),
                            process_id: envelope_process_id,
                            event: ActiveExecutionEvent::JavascriptSyncRpcCompletion(
                                crate::state::JavascriptSyncRpcCompletion { request_id, result },
                            ),
                        })
                        .await
                        .is_err()
                    {
                        eprintln!(
                            "ERR_AGENTOS_PROCESS_EVENT_CHANNEL_CLOSED: deferred sync RPC completion could not be delivered"
                        );
                    } else {
                        event_notify.notify_one();
                    }
                })
                .map_err(SidecarError::from)?;
                return Ok(());
            }
            other => other,
        };

        if response.is_ok() && javascript_sync_rpc_may_make_fd_readable(&request) {
            if let Some(mut vm) = self.vms.get_mut(vm_id) {
                Self::wake_ready_deferred_fd_reads(&mut vm)?;
            }
        }
        if response.is_ok() && javascript_sync_rpc_may_make_fd_writable(&request) {
            if let Some(mut vm) = self.vms.get_mut(vm_id) {
                Self::wake_ready_deferred_fd_writes(&mut vm)?;
            }
        }

        let Some(mut vm) = self.vms.get_mut(vm_id) else {
            log_stale_process_event(
                &self.bridge,
                vm_id,
                process_id,
                "javascript sync RPC response delivery",
            );
            return Ok(());
        };
        let shadow_root = vm.cwd.clone();
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            log_stale_process_event(
                &self.bridge,
                vm_id,
                process_id,
                "javascript sync RPC response delivery",
            );
            return Ok(());
        };

        if response.is_ok()
            && matches!(
                request.method.as_str(),
                "fs.chmodSync" | "fs.promises.chmod"
            )
        {
            let guest_path =
                javascript_sync_rpc_arg_str(&request.args, 0, "filesystem chmod path")?;
            let mode =
                javascript_sync_rpc_arg_u32(&request.args, 1, "filesystem chmod mode")? & 0o7777;
            let host_path =
                shadow_host_path_for_process(&shadow_root, &process.guest_cwd, guest_path);
            if host_path.exists() {
                fs::set_permissions(&host_path, fs::Permissions::from_mode(mode)).map_err(
                    |error| {
                        SidecarError::Io(format!(
                            "failed to mirror chmod to shadow path {}: {error}",
                            host_path.display()
                        ))
                    },
                )?;
            }
        }

        match response {
            Ok(result) => process
                .execution
                .respond_javascript_sync_rpc_response(request.id, result)
                .or_else(ignore_stale_javascript_sync_rpc_response),
            Err(error) => {
                tracing::warn!(
                    method = %request.method,
                    error = %error,
                    "JavaScript sync RPC failed"
                );
                process
                    .execution
                    .respond_javascript_sync_rpc_error(
                        request.id,
                        javascript_sync_rpc_error_code(&error),
                        error.to_string(),
                    )
                    .or_else(ignore_stale_javascript_sync_rpc_response)
            }
        }
    }

    /// Applies a `process.kill` aimed at the calling process itself and
    /// returns the self-delivery action payload for the bridge.
    fn apply_self_process_kill(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parsed_signal: i32,
    ) -> Result<Value, SidecarError> {
        let action = self
            .vms
            .get(vm_id)
            .and_then(|vm| {
                vm.signal_states
                    .get(process_id)
                    .and_then(|handlers| handlers.get(&(parsed_signal as u32)))
                    .map(|registration| registration.action.clone())
            })
            .unwrap_or(SignalDispositionAction::Default);
        if action == SignalDispositionAction::Default
            && parsed_signal != 0
            && !matches!(
                canonical_signal_name(parsed_signal),
                Some("SIGWINCH" | "SIGCHLD" | "SIGCONT" | "SIGURG")
            )
        {
            if let Some(mut vm) = self.vms.get_mut(vm_id) {
                let VmState {
                    kernel,
                    active_processes,
                    ..
                } = &mut *vm;
                if let Some(process) = active_processes.get_mut(process_id) {
                    apply_active_process_default_signal(kernel, process, parsed_signal)?;
                }
            }
        }
        Ok(json!({
            "self": true,
            "action": match action {
                SignalDispositionAction::Default => "default",
                SignalDispositionAction::Ignore => "ignore",
                SignalDispositionAction::User => "user",
            },
        }))
    }

    pub(crate) fn vm_ids_for_scope(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<Vec<String>, SidecarError> {
        match ownership {
            OwnershipScope::SessionOwnership(inner) => {
                self.require_owned_session(&inner.connection_id, &inner.session_id)?;
                Ok(self
                    .sessions
                    .get(&inner.session_id)
                    .expect("owned session should exist")
                    .vm_ids
                    .iter()
                    .cloned()
                    .collect())
            }
            OwnershipScope::VmOwnership(inner) => {
                self.require_owned_vm(&inner.connection_id, &inner.session_id, &inner.vm_id)?;
                Ok(vec![inner.vm_id.clone()])
            }
            OwnershipScope::ConnectionOwnership(..) => Err(SidecarError::InvalidState(
                String::from("event polling requires session or VM ownership scope"),
            )),
        }
    }

    pub(crate) fn vm_ownership(&self, vm_id: &str) -> Result<OwnershipScope, SidecarError> {
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        Ok(OwnershipScope::vm(&vm.connection_id, &vm.session_id, vm_id))
    }

    pub(crate) fn vm_has_active_processes(&self, vm_id: &str) -> bool {
        self.vms
            .get(vm_id)
            .is_some_and(|vm| !vm.active_processes.is_empty())
    }

    fn require_authenticated_connection(&self, connection_id: &str) -> Result<(), SidecarError> {
        if self.connections.contains_key(connection_id) {
            Ok(())
        } else {
            Err(SidecarError::InvalidState(format!(
                "connection {connection_id} has not authenticated"
            )))
        }
    }

    pub(crate) fn require_owned_session(
        &self,
        connection_id: &str,
        session_id: &str,
    ) -> Result<(), SidecarError> {
        self.require_authenticated_connection(connection_id)?;
        let session = self.sessions.get(session_id).ok_or_else(|| {
            SidecarError::InvalidState(format!("unknown sidecar session {session_id}"))
        })?;
        if session.connection_id == connection_id {
            Ok(())
        } else {
            Err(SidecarError::InvalidState(format!(
                "session {session_id} is not owned by connection {connection_id}"
            )))
        }
    }

    pub(crate) fn require_owned_vm(
        &self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
    ) -> Result<(), SidecarError> {
        self.require_owned_session(connection_id, session_id)?;
        if let Some(quarantined) = self.quarantined_vms.values().find(|quarantined| {
            quarantined.vm_id == vm_id
                && quarantined.connection_id == connection_id
                && quarantined.session_id == session_id
        }) {
            let snapshot = quarantined.reconciliation_snapshot();
            return Err(SidecarError::InvalidState(format!(
                "ERR_AGENTOS_VM_QUARANTINED: vm_id={vm_id} generation={} reason={:?} active_tasks={} outstanding_capabilities={} ledger_zero={} integrity_ok={}",
                quarantined.generation,
                quarantined.reason,
                snapshot.active_tasks,
                snapshot.outstanding_capabilities,
                snapshot.ledger_zero,
                snapshot.integrity_ok
            )));
        }
        let vm = self
            .vms
            .get(vm_id)
            .ok_or_else(|| SidecarError::InvalidState(format!("unknown sidecar VM {vm_id}")))?;
        if vm.connection_id != connection_id || vm.session_id != session_id {
            return Err(SidecarError::InvalidState(format!(
                "VM {vm_id} is not owned by {connection_id}/{session_id}"
            )));
        }
        Ok(())
    }

    fn connection_id_for(&self, ownership: &OwnershipScope) -> Result<String, SidecarError> {
        match ownership {
            OwnershipScope::ConnectionOwnership(inner) => Ok(inner.connection_id.clone()),
            OwnershipScope::SessionOwnership(..) | OwnershipScope::VmOwnership(..) => {
                Err(SidecarError::InvalidState(String::from(
                    "request requires connection ownership scope",
                )))
            }
        }
    }

    fn validate_auth_token(&self, auth_token: &str) -> Result<(), SidecarError> {
        let Some(expected_auth_token) = self.config.expected_auth_token.as_deref() else {
            return Ok(());
        };

        if auth_token == expected_auth_token {
            Ok(())
        } else {
            Err(SidecarError::Unauthorized(String::from(
                "authenticate request provided an invalid auth token",
            )))
        }
    }

    fn allocate_connection_id(&mut self) -> String {
        self.next_connection_id += 1;
        format!("conn-{}", self.next_connection_id)
    }

    fn take_matching_process_event_envelope(
        &mut self,
        vm_id: &str,
        process_id: &str,
    ) -> Result<Option<ProcessEventEnvelope>, SidecarError> {
        if let Some(index) = self
            .pending_process_events
            .iter()
            .position(|event| event.vm_id == vm_id && event.process_id == process_id)
        {
            let envelope = self.pending_process_events.remove(index);
            self.observe_pending_process_event_depth();
            self.rearm_deferred_process_event_after_capacity_release();
            return Ok(envelope);
        }

        let mut matching_envelope = None;
        let mut deferred = Vec::new();
        {
            let pending_capacity = self.pending_process_event_capacity();
            let receiver = self.process_event_receiver.as_mut().ok_or_else(|| {
                SidecarError::InvalidState(String::from("process event receiver unavailable"))
            })?;
            loop {
                if deferred.len() >= pending_capacity {
                    if receiver.is_empty() {
                        break;
                    }
                    return Err(process_event_queue_overflow_error(
                        self.config.runtime.protocol.max_process_events,
                    ));
                }
                let envelope = match receiver.try_recv() {
                    Ok(envelope) => envelope,
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                };
                if matching_envelope.is_none()
                    && envelope.vm_id == vm_id
                    && envelope.process_id == process_id
                {
                    matching_envelope = Some(envelope);
                    break;
                }
                deferred.push(envelope);
            }
        }
        for envelope in deferred {
            self.queue_pending_process_event(envelope)?;
        }

        Ok(matching_envelope)
    }

    fn allocate_sidecar_request_id(&mut self) -> RequestId {
        let request_id = self.next_sidecar_request_id;
        self.next_sidecar_request_id -= 1;
        request_id
    }

    pub(crate) fn session_scope_for(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<(String, String), SidecarError> {
        match ownership {
            OwnershipScope::SessionOwnership(inner) => {
                Ok((inner.connection_id.clone(), inner.session_id.clone()))
            }
            OwnershipScope::ConnectionOwnership(..) | OwnershipScope::VmOwnership(..) => {
                Err(SidecarError::InvalidState(String::from(
                    "request requires session ownership scope",
                )))
            }
        }
    }

    pub(crate) fn vm_scope_for(
        &self,
        ownership: &OwnershipScope,
    ) -> Result<(String, String, String), SidecarError> {
        match ownership {
            OwnershipScope::VmOwnership(inner) => Ok((
                inner.connection_id.clone(),
                inner.session_id.clone(),
                inner.vm_id.clone(),
            )),
            OwnershipScope::ConnectionOwnership(..) | OwnershipScope::SessionOwnership(..) => Err(
                SidecarError::InvalidState(String::from("request requires VM ownership scope")),
            ),
        }
    }

    pub(crate) fn respond(
        &self,
        request: &RequestFrame,
        payload: ResponsePayload,
    ) -> ResponseFrame {
        shared_respond(request, payload)
    }

    pub(crate) fn reject(
        &self,
        request: &RequestFrame,
        code: &str,
        message: &str,
    ) -> ResponseFrame {
        shared_reject(request, code, message)
    }

    fn reject_error(&self, request: &RequestFrame, error: &SidecarError) -> ResponseFrame {
        let SidecarError::ResourceLimit(limit) = error else {
            return self.reject(request, error_code(error), &error.to_string());
        };
        use agentos_runtime::accounting::ResourceClass;

        // A child VM ledger can fail because its process parent is full. Do not
        // return that parent ledger's exact occupancy to an untrusted guest:
        // it would be a cross-VM resource-usage oracle. VM-local usage remains
        // useful and safe to report; process pressure is identified by scope,
        // limit and configuration path without the aggregate `used` value.
        let guest_limit = guest_limit_diagnostic(limit);

        let vm_id = match &request.ownership {
            OwnershipScope::VmOwnership(owner) => Some(owner.vm_id.clone()),
            OwnershipScope::ConnectionOwnership(_) | OwnershipScope::SessionOwnership(_) => None,
        };
        let session_generation = vm_id
            .as_ref()
            .and_then(|vm_id| self.vms.get(vm_id))
            .map(|vm| vm.generation);
        let unit = match limit.resource {
            ResourceClass::BufferedBytes
            | ResourceClass::HandleCommandBytes
            | ResourceClass::BridgeRequestBytes
            | ResourceClass::BridgeResponseBytes
            | ResourceClass::AsyncCompletionBytes
            | ResourceClass::UdpBytes
            | ResourceClass::TlsBytes
            | ResourceClass::ExecutorBytes
            | ResourceClass::Http2BufferedBytes
            | ResourceClass::Http2HeaderBytes
            | ResourceClass::Http2DataBytes
            | ResourceClass::Http2CommandBytes
            | ResourceClass::Http2EventBytes => "bytes",
            ResourceClass::Tasks => "tasks",
            ResourceClass::Timers => "timers",
            ResourceClass::Connections | ResourceClass::Http2Connections => "connections",
            ResourceClass::Http2Streams => "streams",
            ResourceClass::ExecutorSlots => "workers",
            ResourceClass::Capabilities
            | ResourceClass::ReadyHandles
            | ResourceClass::Sockets
            | ResourceClass::Datagrams
            | ResourceClass::HandleCommands
            | ResourceClass::BridgeCalls
            | ResourceClass::AsyncCompletions
            | ResourceClass::UdpDatagrams
            | ResourceClass::Http2Commands
            | ResourceClass::Http2Events => "items",
        };
        let errno = match limit.resource {
            ResourceClass::Capabilities | ResourceClass::Sockets => "EMFILE",
            _ => "ENOBUFS",
        };
        self.respond(
            request,
            ResponsePayload::Rejected(RejectedResponse {
                code: String::from("ERR_AGENTOS_RESOURCE_LIMIT"),
                message: guest_limit.message,
                limit_name: Some(limit.resource.name().to_owned()),
                configured_limit: Some(u64::try_from(limit.limit).unwrap_or(u64::MAX)),
                current_usage: guest_limit.current_usage,
                requested: Some(u64::try_from(limit.requested).unwrap_or(u64::MAX)),
                unit: Some(unit.to_owned()),
                scope: Some(String::from(guest_limit.scope)),
                vm_id,
                session_generation,
                capability_id: None,
                operation: None,
                configuration_path: Some(limit.config_path.clone()),
                retryable: Some(false),
                errno: Some(errno.to_owned()),
            }),
        )
    }

    pub fn queue_sidecar_request(
        &mut self,
        ownership: OwnershipScope,
        payload: SidecarRequestPayload,
    ) -> Result<RequestId, SidecarError> {
        let outbound_limit = self.config.runtime.protocol.max_outbound_requests;
        if self.outbound_sidecar_requests.len() >= outbound_limit {
            return Err(outbound_sidecar_request_queue_overflow_error(
                outbound_limit,
            ));
        }
        let pending_limit = self.config.runtime.protocol.max_pending_responses;
        if self.pending_sidecar_responses.pending_count() >= pending_limit {
            return Err(sidecar_response_pending_overflow_error(pending_limit));
        }
        let request_id = self.allocate_sidecar_request_id();
        let request = SidecarRequestFrame::new(request_id, ownership, payload);
        self.pending_sidecar_responses
            .register_request(&request)
            .map_err(sidecar_response_tracker_error)?;
        self.outbound_sidecar_requests.push_back(request);
        self.outbound_sidecar_requests_gauge
            .observe_depth(self.outbound_sidecar_requests.len());
        self.pending_sidecar_responses_gauge
            .observe_depth(self.pending_sidecar_responses.pending_count());
        Ok(request_id)
    }

    pub fn queue_wire_sidecar_request(
        &mut self,
        ownership: crate::wire::OwnershipScope,
        payload: crate::wire::SidecarRequestPayload,
    ) -> Result<crate::wire::RequestId, SidecarError> {
        let ownership = crate::wire::ownership_scope_to_compat(ownership);
        let payload = crate::wire::sidecar_request_payload_to_compat(&ownership, payload)
            .map_err(wire_protocol_error)?;
        self.queue_sidecar_request(ownership, payload)
    }

    pub fn pop_sidecar_request(&mut self) -> Option<SidecarRequestFrame> {
        let request = self.outbound_sidecar_requests.pop_front();
        self.outbound_sidecar_requests_gauge
            .observe_depth(self.outbound_sidecar_requests.len());
        request
    }

    pub fn pop_wire_sidecar_request(
        &mut self,
    ) -> Result<Option<crate::wire::SidecarRequestFrame>, SidecarError> {
        self.pop_sidecar_request()
            .map(crate::wire::sidecar_request_frame_from_compat)
            .transpose()
            .map_err(wire_protocol_error)
    }

    pub fn accept_sidecar_response(
        &mut self,
        response: SidecarResponseFrame,
    ) -> Result<(), SidecarError> {
        match self.pending_sidecar_responses.accept_response(&response) {
            Ok(()) => {}
            // A response for a request that is no longer pending (its owning VM
            // was disposed, abandoning the in-flight callback) or already
            // completed is a benign late/stale reply on the shared sidecar — a
            // per-VM `sidecar_request` can be answered by the host after that VM
            // has been torn down (multiple VMs share one sidecar process). Drop
            // it instead of failing the whole sidecar over a harmless straggler.
            Err(
                error @ (SidecarResponseTrackerError::UnmatchedResponse { .. }
                | SidecarResponseTrackerError::DuplicateResponse { .. }),
            ) => {
                tracing::warn!(
                    request_id = response.request_id,
                    "dropping stale sidecar response with no matching pending request: {error}"
                );
                return Ok(());
            }
            Err(error) => return Err(sidecar_response_tracker_error(error)),
        }
        self.pending_sidecar_responses_gauge
            .observe_depth(self.pending_sidecar_responses.pending_count());
        self.completed_sidecar_response_order
            .push_back(response.request_id);
        self.completed_sidecar_responses
            .insert(response.request_id, response);
        self.completed_sidecar_responses_gauge
            .observe_depth(self.completed_sidecar_responses.len());
        let completed_limit = self.config.runtime.protocol.max_completed_responses;
        while self.completed_sidecar_responses.len() > completed_limit {
            match self.completed_sidecar_response_order.pop_front() {
                // Only a response that was never retrieved is a real loss; an id
                // already taken via take_sidecar_response leaves a stale order
                // entry that removes to None and is not a dropped response.
                Some(evicted) => {
                    if self.completed_sidecar_responses.remove(&evicted).is_some() {
                        tracing::warn!(
                            code = "WARN_AGENTOS_COMPLETED_RESPONSE_LIMIT",
                            queue = "completed_sidecar_responses",
                            evicted_request_id = evicted,
                            capacity = completed_limit,
                            configuration_path = "runtime.protocol.maxCompletedResponses",
                            "dropping an unretrieved completed sidecar response to stay within configured cap; raise runtime.protocol.maxCompletedResponses to retain more completions (response lost)"
                        );
                        self.completed_sidecar_responses_gauge
                            .observe_depth(self.completed_sidecar_responses.len());
                    }
                }
                None => break,
            }
        }
        Ok(())
    }

    pub fn accept_wire_sidecar_response(
        &mut self,
        response: crate::wire::SidecarResponseFrame,
    ) -> Result<(), SidecarError> {
        let response =
            crate::wire::sidecar_response_frame_to_compat(response).map_err(wire_protocol_error)?;
        self.accept_sidecar_response(response)
    }

    pub fn take_sidecar_response(&mut self, request_id: RequestId) -> Option<SidecarResponseFrame> {
        let response = self.completed_sidecar_responses.remove(&request_id);
        if response.is_some() {
            self.completed_sidecar_response_order
                .retain(|completed_id| completed_id != &request_id);
            self.completed_sidecar_responses_gauge
                .observe_depth(self.completed_sidecar_responses.len());
        }
        response
    }

    pub fn take_wire_sidecar_response(
        &mut self,
        request_id: crate::wire::RequestId,
    ) -> Result<Option<crate::wire::SidecarResponseFrame>, SidecarError> {
        self.take_sidecar_response(request_id)
            .map(|response| {
                crate::wire::sidecar_response_frame_from_compat(response)
                    .map_err(wire_protocol_error)
            })
            .transpose()
    }

    pub(crate) fn vm_lifecycle_event(
        &self,
        connection_id: &str,
        session_id: &str,
        vm_id: &str,
        state: VmLifecycleState,
    ) -> EventFrame {
        shared_vm_lifecycle_event(connection_id, session_id, vm_id, state)
    }

    fn ensure_request_within_frame_limit(
        &self,
        request: &RequestFrame,
    ) -> Result<(), SidecarError> {
        let frame = crate::protocol::to_generated_protocol_frame(
            &crate::protocol::ProtocolFrame::Request(request.clone()),
        )
        .map_err(|error| {
            SidecarError::InvalidState(format!("failed to convert request frame: {error}"))
        })?;
        let crate::wire::ProtocolFrame::RequestFrame(_) = &frame else {
            return Err(SidecarError::InvalidState(String::from(
                "request converted to non-request wire frame",
            )));
        };

        crate::wire::WireFrameCodec::new(self.config.max_frame_bytes)
            .encode(&frame)
            .map(|_| ())
            .map_err(|error| SidecarError::FrameTooLarge(error.to_string()))
    }
}

impl<B> Drop for NativeSidecar<B> {
    fn drop(&mut self) {
        if let Some(task) = self.kernel_reaper_task.take() {
            task.abort();
        }
        if let Err(error) = self
            .process_event_broker
            .shutdown(OperationCancellationReason::Shutdown)
        {
            eprintln!("ERR_AGENTOS_PROCESS_EVENT_BROKER_SHUTDOWN: {error}");
        }
    }
}

impl<B> ExtensionHost for NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    fn acp_termination_grace<'a>(&'a mut self) -> ExtensionFuture<'a, Duration> {
        Box::pin(async move { Ok(self.config.acp_termination_grace) })
    }

    fn vm_acp_limits<'a>(
        &'a mut self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, agentos_native_sidecar_core::limits::AcpLimits> {
        Box::pin(async move {
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
            self.vms
                .get(&vm_id)
                .map(|vm| vm.limits.acp.clone())
                .ok_or_else(|| SidecarError::InvalidState(format!("VM not found: {vm_id}")))
        })
    }

    fn vm_database<'a>(
        &'a mut self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, Option<crate::vm_sqlite::SharedVmSqliteDatabase>> {
        Box::pin(async move {
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
            Ok(self.vms.get(&vm_id).and_then(|vm| vm.database.clone()))
        })
    }

    fn spawn_process<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        payload: ExecuteRequest,
    ) -> ExtensionFuture<'a, ProcessStartedResponse> {
        Box::pin(async move {
            let request = RequestFrame::new(0, ownership, RequestPayload::Execute(payload.clone()));
            let dispatch = NativeSidecar::execute(self, &request, payload).await?;
            match dispatch.response.payload {
                ResponsePayload::ProcessStarted(response) => Ok(response),
                other => Err(unexpected_extension_host_response("execute", other)),
            }
        })
    }

    fn write_stdin<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        payload: WriteStdinRequest,
    ) -> ExtensionFuture<'a, StdinWrittenResponse> {
        Box::pin(async move {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::WriteStdin(payload.clone()));
            let dispatch = NativeSidecar::write_stdin(self, &request, payload).await?;
            match dispatch.response.payload {
                ResponsePayload::StdinWritten(response) => Ok(response),
                other => Err(unexpected_extension_host_response("write_stdin", other)),
            }
        })
    }

    fn close_stdin<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        payload: CloseStdinRequest,
    ) -> ExtensionFuture<'a, StdinClosedResponse> {
        Box::pin(async move {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::CloseStdin(payload.clone()));
            let dispatch = NativeSidecar::close_stdin(self, &request, payload).await?;
            match dispatch.response.payload {
                ResponsePayload::StdinClosed(response) => Ok(response),
                other => Err(unexpected_extension_host_response("close_stdin", other)),
            }
        })
    }

    fn kill_process<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        payload: KillProcessRequest,
    ) -> ExtensionFuture<'a, ProcessKilledResponse> {
        Box::pin(async move {
            let request =
                RequestFrame::new(0, ownership, RequestPayload::KillProcess(payload.clone()));
            let dispatch = NativeSidecar::kill_process(self, &request, payload).await?;
            match dispatch.response.payload {
                ResponsePayload::ProcessKilled(response) => Ok(response),
                other => Err(unexpected_extension_host_response("kill_process", other)),
            }
        })
    }

    fn poll_event<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        timeout: Duration,
    ) -> ExtensionFuture<'a, Option<EventFrame>> {
        Box::pin(async move { NativeSidecar::poll_event(self, &ownership, timeout).await })
    }

    fn poll_process_event<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        process_id: String,
        timeout: Duration,
    ) -> ExtensionFuture<'a, Option<EventFrame>> {
        Box::pin(async move {
            NativeSidecar::poll_process_event(self, &ownership, &process_id, timeout).await
        })
    }

    fn projected_agents<'a>(
        &'a mut self,
        ownership: OwnershipScope,
    ) -> ExtensionFuture<'a, Vec<crate::extension::ProjectedAgentLaunchEntry>> {
        Box::pin(async move {
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
            let vm = self
                .vms
                .get(&vm_id)
                .ok_or_else(|| SidecarError::InvalidState(format!("unknown VM {vm_id}")))?;
            Ok(vm
                .projected_agent_launch
                .iter()
                .map(
                    |(id, launch): (&String, &crate::state::ProjectedAgentLaunch)| {
                        crate::extension::ProjectedAgentLaunchEntry {
                            id: id.clone(),
                            acp_entrypoint: launch.acp_entrypoint.clone(),
                            env: launch.env.clone(),
                            launch_args: launch.launch_args.clone(),
                        }
                    },
                )
                .collect())
        })
    }

    fn guest_filesystem_call<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        payload: GuestFilesystemCallRequest,
    ) -> ExtensionFuture<'a, GuestFilesystemResultResponse> {
        Box::pin(async move {
            let request = RequestFrame::new(
                0,
                ownership,
                RequestPayload::GuestFilesystemCall(payload.clone()),
            );
            let dispatch = NativeSidecar::guest_filesystem_call(self, &request, payload).await?;
            match dispatch.response.payload {
                ResponsePayload::GuestFilesystemResult(response) => Ok(response),
                other => Err(unexpected_extension_host_response(
                    "guest_filesystem_call",
                    other,
                )),
            }
        })
    }

    fn bind_process_to_session<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            self.bind_extension_process_resource(ownership, namespace, ext_session_id, process_id)
        })
    }

    fn bind_vm_to_session<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(
            async move { self.bind_extension_vm_resource(ownership, namespace, ext_session_id) },
        )
    }

    fn dispose_session_resources<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
    ) -> ExtensionFuture<'a, Vec<EventFrame>> {
        Box::pin(async move {
            let key = (namespace, ext_session_id);
            let Some(resources) = self.extension_sessions.get(&key) else {
                return Ok(Vec::new());
            };
            if resources.ownership != ownership {
                return Err(SidecarError::InvalidState(String::from(
                    "extension session ownership did not match dispose request",
                )));
            }
            let resources = self
                .extension_sessions
                .remove(&key)
                .expect("extension resources existed before removal");
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            for process_id in resources.process_ids {
                if self
                    .vms
                    .get(&vm_id)
                    .is_some_and(|vm| vm.active_processes.contains_key(&process_id))
                {
                    self.kill_process_internal(&vm_id, &process_id, "SIGTERM")?;
                }
            }
            let mut events = Vec::new();
            for resource_vm_id in resources.vm_ids {
                if self.vms.contains_key(&resource_vm_id) {
                    events.extend(
                        self.dispose_vm_internal(
                            &connection_id,
                            &session_id,
                            &resource_vm_id,
                            DisposeReason::Requested,
                        )
                        .await?,
                    );
                }
            }
            Ok(events)
        })
    }

    fn start_buffering_process_output<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        process_id: String,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
            let key = (vm_id, process_id);
            if self.extension_process_output_buffers.contains_key(&key) {
                return Err(SidecarError::Conflict(String::from(
                    "extension process output buffering already started",
                )));
            }
            self.extension_process_output_buffers
                .insert(key, ExtensionBufferedProcessOutput::default());
            Ok(())
        })
    }

    fn handoff_buffered_process_output<'a>(
        &'a mut self,
        ownership: OwnershipScope,
        namespace: String,
        ext_session_id: String,
        process_id: String,
        timeout: Duration,
    ) -> ExtensionFuture<'a, ExtensionBufferedProcessOutput> {
        Box::pin(async move {
            let deadline = Instant::now() + timeout;
            let process_event_notify = Arc::clone(&self.process_event_notify);
            loop {
                let notified = process_event_notify.notified();
                let finalize = timeout.is_zero() || Instant::now() >= deadline;
                if let Some(buffer) = self
                    .probe_extension_process_output_handoff(
                        ownership.clone(),
                        namespace.clone(),
                        ext_session_id.clone(),
                        process_id.clone(),
                        finalize,
                    )
                    .await?
                {
                    return Ok(buffer);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = notified => {}
                    _ = time::sleep(remaining) => {}
                }
            }
        })
    }
}

fn unexpected_extension_host_response(operation: &str, payload: ResponsePayload) -> SidecarError {
    match payload {
        ResponsePayload::Rejected(response) => SidecarError::InvalidState(format!(
            "extension {operation} rejected with {}: {}",
            response.code, response.message
        )),
        other => SidecarError::InvalidState(format!(
            "extension {operation} returned unexpected response: {other:?}"
        )),
    }
}

fn shadow_host_path_for_process(
    shadow_root: &Path,
    process_guest_cwd: &str,
    guest_path: &str,
) -> PathBuf {
    let normalized_guest_path = if guest_path.starts_with('/') {
        normalize_path(guest_path)
    } else {
        normalize_path(&format!(
            "{}/{}",
            process_guest_cwd.trim_end_matches('/'),
            guest_path
        ))
    };
    if normalized_guest_path == "/" {
        shadow_root.to_path_buf()
    } else {
        shadow_root.join(normalized_guest_path.trim_start_matches('/'))
    }
}

fn sidecar_response_tracker_error(error: SidecarResponseTrackerError) -> SidecarError {
    SidecarError::InvalidState(format!(
        "invalid sidecar response correlation state: {error}"
    ))
}

fn map_bridge_permission(decision: agentos_bridge::PermissionDecision) -> PermissionDecision {
    match decision.verdict {
        agentos_bridge::PermissionVerdict::Allow => PermissionDecision::allow(),
        agentos_bridge::PermissionVerdict::Deny => PermissionDecision::deny(
            decision
                .reason
                .unwrap_or_else(|| String::from("denied by host")),
        ),
        agentos_bridge::PermissionVerdict::Prompt => PermissionDecision::deny(
            decision
                .reason
                .unwrap_or_else(|| String::from("permission prompt required")),
        ),
    }
}

fn audit_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis()
        .to_string()
}

pub(crate) fn audit_fields<I, K, V>(fields: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut mapped = BTreeMap::from([(String::from("timestamp"), audit_timestamp())]);
    for (key, value) in fields {
        mapped.insert(key.into(), value.into());
    }
    mapped
}

pub(crate) fn emit_structured_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    name: &str,
    fields: BTreeMap<String, String>,
) -> Result<(), SidecarError>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    bridge.with_mut(|bridge| {
        bridge.emit_structured_event(StructuredEventRecord {
            vm_id: vm_id.to_owned(),
            name: name.to_owned(),
            fields,
        })
    })
}

pub(crate) fn emit_security_audit_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    name: &str,
    fields: BTreeMap<String, String>,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    emit_structured_event_or_stderr(bridge, vm_id, name, fields);
}

pub(crate) fn emit_structured_event_or_stderr<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    name: &str,
    fields: BTreeMap<String, String>,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    if let Err(error) = emit_structured_event(bridge, vm_id, name, fields) {
        // This fallback must remain independent of bridge telemetry: routing
        // the failure through the same bridge can recurse or hide it again.
        eprintln!(
            "ERR_AGENTOS_STRUCTURED_EVENT: vm_id={vm_id} event={name} delivery failed: {error}"
        );
    }
}

/// Build a wire `EventFrame` carrying a `StructuredEvent` (name + string-map
/// detail) scoped to a connection. Used to forward limit-registry warnings to the
/// host as `{type:"structured", name:"limit_warning", detail}` events without a
/// protocol schema change. Emitted directly to the host (not via the polled,
/// per-session bridge queue, which is a no-op in the stdio sidecar), so a
/// process-global signal is delivered against the active connection.
pub(crate) fn structured_event_frame(
    connection_id: &str,
    name: &str,
    detail: std::collections::HashMap<String, String>,
) -> Result<crate::wire::EventFrame, SidecarError> {
    let event = EventFrame::new(
        OwnershipScope::connection(connection_id),
        EventPayload::Structured(crate::protocol::StructuredEvent {
            name: name.to_owned(),
            detail,
        }),
    );
    crate::wire::event_frame_from_compat(event).map_err(|error| {
        SidecarError::InvalidState(format!("invalid structured event frame: {error}"))
    })
}

pub(crate) fn log_stale_process_event<B>(
    bridge: &SharedBridge<B>,
    vm_id: &str,
    process_id: &str,
    context: &str,
) where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
    let _ = bridge.emit_log(
        vm_id,
        format!(
            "Ignoring stale process event during {context}: VM {vm_id} process {process_id} was already reaped"
        ),
    );
}

// filesystem_operation_label moved to crate::vm

pub(crate) fn root_filesystem_error(error: impl std::fmt::Display) -> SidecarError {
    SidecarError::InvalidState(format!("root filesystem: {error}"))
}

pub(crate) fn normalize_path(path: &str) -> String {
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir => segments.clear(),
            Component::ParentDir => {
                segments.pop();
            }
            Component::CurDir => {}
            Component::Normal(value) => segments.push(value.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                segments.push(prefix.as_os_str().to_string_lossy().into_owned());
            }
        }
    }

    let normalized = format!("/{}", segments.join("/"));
    if normalized.is_empty() {
        String::from("/")
    } else {
        normalized
    }
}

pub(crate) fn normalize_host_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if normalized != Path::new("/") {
                    normalized.pop();
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    if normalized.as_os_str().is_empty() {
        if path.is_absolute() {
            PathBuf::from("/")
        } else {
            PathBuf::from(".")
        }
    } else {
        normalized
    }
}

pub(crate) fn path_is_within_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub(crate) fn dirname(path: &str) -> String {
    let normalized = normalize_path(path);
    let parent = Path::new(&normalized)
        .parent()
        .unwrap_or_else(|| Path::new("/"));
    let value = parent.to_string_lossy();
    if value.is_empty() {
        String::from("/")
    } else {
        value.into_owned()
    }
}

pub(crate) fn kernel_error(error: KernelError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}

pub(crate) fn plugin_error(error: PluginError) -> SidecarError {
    SidecarError::Plugin(error.to_string())
}

pub(crate) fn javascript_error(error: JavascriptExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn wasm_error(error: WasmExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn python_error(error: PythonExecutionError) -> SidecarError {
    SidecarError::Execution(error.to_string())
}

pub(crate) fn vfs_error(error: VfsError) -> SidecarError {
    SidecarError::Kernel(error.to_string())
}

/// Actionable guidance shown when guest package resolution fails because the packages live in a
/// non-flat `node_modules` whose package store is not visible in the VM. Mounting host `node_modules`
/// is a bind mount, so symlinked/store layouts
/// do not resolve inside the VM: Node canonicalizes a module to its store
/// realpath (e.g. `node_modules/.pnpm/...`, `.bun/...`, `.store/...`) which lives
/// above the mounted directory and the guest `fs` cannot read. Plug'n'Play
/// (yarn-berry default) has no `node_modules` at all. A flat (hoisted) layout is
/// required. The empirically-supported package managers are captured in
/// `crates/sidecar/tests/module_layout_e2e.rs`.
#[allow(dead_code)]
const HOISTED_NODE_MODULES_GUIDANCE: &str = "agentos can't load mounted node_modules: the directory uses a non-flat layout (pnpm / bun / yarn workspaces store, or yarn Plug'n'Play) whose package store isn't visible inside the VM. A flat (hoisted) node_modules is required.\n  - pnpm        -> add `node-linker=hoisted` to .npmrc, then reinstall\n  - yarn berry  -> set `nodeLinker: node-modules` in .yarnrc.yml (not pnp/pnpm)\n  - bun         -> install dependencies outside a workspace (workspaces use a .bun store)\n  - npm / yarn classic -> already flat, no change needed";

/// Detect, from an adapter's captured stderr, a non-flat-`node_modules` failure
/// signature. Returns the actionable guidance to fold into the surfaced error,
/// or `None` when the failure is unrelated.
///
/// Two signatures, both kept specific so they never fire on unrelated crashes:
/// - a missing-file / cannot-resolve error referencing a package STORE path that
///   lives above the mounted project (`.pnpm`, `.bun`, `.store`, PnP `__virtual__`),
/// - a yarn Plug'n'Play fingerprint (`.pnp.cjs`, the zip cache, or PnP's
///   "isn't declared in your dependencies" resolver error).
#[allow(dead_code)]
fn symlinked_node_modules_hint(stderr: &str) -> Option<&'static str> {
    // Package stores that only appear in a path when a non-flat layout is used.
    // pnpm (isolated), bun (workspace), yarn-berry (nodeLinker: pnpm), and PnP
    // virtual instances all keep real package files under these store dirs, which
    // sit above the mounted project node_modules and so are not guest-visible.
    const STORE_MARKERS: &[&str] = &[
        "node_modules/.pnpm/",
        "node_modules/.bun/",
        "node_modules/.store/",
        "/__virtual__/",
    ];
    // Yarn Plug'n'Play has no node_modules at all; resolution fails against the
    // .pnp runtime / zip cache. "isn't declared in your dependencies" is PnP's
    // distinctive resolver error and is specific enough to fire on its own.
    const PNP_STRICT_MARKERS: &[&str] = &["isn't declared in your dependencies"];
    const PNP_PATH_MARKERS: &[&str] = &[".pnp.cjs", ".pnp.loader.mjs", "/.yarn/cache/"];

    if PNP_STRICT_MARKERS.iter().any(|m| stderr.contains(m)) {
        return Some(HOISTED_NODE_MODULES_GUIDANCE);
    }

    let missing = stderr.contains("ENOENT")
        || stderr.contains("no such file or directory")
        || stderr.contains("Cannot find module")
        || stderr.contains("MODULE_NOT_FOUND");
    if !missing {
        return None;
    }
    if STORE_MARKERS.iter().any(|m| stderr.contains(m))
        || PNP_PATH_MARKERS.iter().any(|m| stderr.contains(m))
    {
        return Some(HOISTED_NODE_MODULES_GUIDANCE);
    }
    None
}

#[cfg(test)]
mod prepared_request_tests {
    use super::*;
    use crate::stdio::LocalBridge;

    fn test_sidecar() -> NativeSidecar<LocalBridge> {
        NativeSidecar::new(LocalBridge::default()).expect("build test sidecar")
    }

    fn wire_request(request: RequestFrame) -> crate::wire::RequestFrame {
        match crate::protocol::to_generated_protocol_frame(
            &crate::protocol::ProtocolFrame::Request(request),
        )
        .expect("encode request")
        {
            crate::wire::ProtocolFrame::RequestFrame(request) => request,
            _ => panic!("request encoded as another frame class"),
        }
    }

    fn compat_response(dispatch: crate::wire::WireDispatchResult) -> ResponseFrame {
        match crate::protocol::from_generated_protocol_frame(
            crate::wire::ProtocolFrame::ResponseFrame(dispatch.response),
        )
        .expect("decode response")
        {
            crate::protocol::ProtocolFrame::Response(response) => response,
            _ => panic!("response decoded as another frame class"),
        }
    }

    async fn create_test_vms(
        sidecar: &mut NativeSidecar<LocalBridge>,
        count: usize,
    ) -> Vec<String> {
        let connection_id = "vm-handle-test-connection";
        let session_id = "vm-handle-test-session";
        sidecar.connections.insert(
            connection_id.to_owned(),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([session_id.to_owned()]),
            },
        );
        sidecar.sessions.insert(
            session_id.to_owned(),
            SessionState {
                connection_id: connection_id.to_owned(),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids: BTreeSet::new(),
            },
        );

        let mut vm_ids = Vec::with_capacity(count);
        for request_id in 1..=count {
            let request = RequestFrame::new(
                request_id as RequestId,
                OwnershipScope::session(connection_id, session_id),
                RequestPayload::CreateVm(crate::protocol::CreateVmRequest::legacy_test_config(
                    crate::protocol::GuestRuntimeKind::JavaScript,
                    std::collections::HashMap::new(),
                    Default::default(),
                    Some(crate::wire::PermissionsPolicy::allow_all()),
                )),
            );
            let RequestPayload::CreateVm(payload) = request.payload.clone() else {
                unreachable!("test request is create VM");
            };
            let response = sidecar
                .create_vm(&request, payload)
                .await
                .expect("create test VM");
            let ResponsePayload::VmCreated(created) = response.response.payload else {
                panic!("test VM creation returned a different response");
            };
            vm_ids.push(created.vm_id);
        }
        vm_ids
    }

    fn cleanup_test_ownership(vm_id: &str) -> OwnershipScope {
        OwnershipScope::vm("vm-handle-test-connection", "vm-handle-test-session", vm_id)
    }

    fn extension_tracks_process(
        sidecar: &NativeSidecar<LocalBridge>,
        extension_session_id: &str,
        process_id: &str,
    ) -> bool {
        sidecar
            .extension_sessions
            .get(&(
                String::from("test.cleanup"),
                extension_session_id.to_owned(),
            ))
            .is_some_and(|resources| resources.process_ids.contains(process_id))
    }

    fn insert_bound_resident_cleanup_fixture(
        sidecar: &mut NativeSidecar<LocalBridge>,
        vm_id: &str,
        execution_id: &str,
        process_id: &str,
        context: bool,
        completed: bool,
        expires_at_ms: Option<u64>,
    ) -> String {
        let extension_session_id = format!("ext-{process_id}");
        let mut vm = sidecar.vms.get_mut(vm_id).expect("cleanup test VM");
        let guest_env = vm.guest_env.clone();
        let kernel_handle = vm
            .kernel
            .create_virtual_process(
                EXECUTION_DRIVER_NAME,
                EXECUTION_DRIVER_NAME,
                crate::state::JAVASCRIPT_COMMAND,
                vec![String::from(crate::state::JAVASCRIPT_COMMAND)],
                agentos_kernel::kernel::VirtualProcessOptions {
                    env: guest_env,
                    ..Default::default()
                },
            )
            .expect("spawn cleanup fixture kernel process");
        let kernel_pid = kernel_handle.pid();
        let runtime_context = vm.runtime_context.clone();
        let limits = vm.limits.clone();
        vm.active_processes.insert(
            process_id.to_owned(),
            crate::state::ActiveProcess::new(
                kernel_pid,
                kernel_handle,
                runtime_context,
                limits,
                agentos_runtime::DEFAULT_PROTOCOL_MAX_PROCESS_EVENTS,
                crate::protocol::GuestRuntimeKind::JavaScript,
                crate::state::ActiveExecution::Binding(crate::state::BindingExecution::default()),
            ),
        );
        let descriptor = crate::protocol::ExecutionDescriptor {
            execution_id: execution_id.to_owned(),
            generation: 0,
            state: crate::protocol::ExecutionState::Idle,
            retained_language: None,
            process_id: None,
            pid: None,
            created_at_ms: 1,
            last_started_at_ms: None,
            last_completed_at_ms: completed.then_some(2),
            last_outcome: completed.then_some(crate::protocol::ExecutionOutcome::Succeeded),
            last_exit_code: completed.then_some(0),
        };
        let result = completed.then(|| crate::protocol::ExecutionCompletedResponse {
            execution: Some(descriptor.clone()),
            outcome: crate::protocol::ExecutionOutcome::Succeeded,
            exit_code: Some(0),
            error: None,
            stdout: None,
            stderr: None,
            stdout_truncated: None,
            stderr_truncated: None,
            evaluation_value: None,
            type_script_check_result: None,
        });
        vm.executions.insert(
            execution_id.to_owned(),
            crate::state::ManagedLanguageExecution {
                public: true,
                context,
                descriptor,
                result,
                events: VecDeque::new(),
                retained_event_bytes: 0,
                output_truncated: false,
                next_sequence: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                output_limit_bytes: 0,
                output_limit_setting: "limits.execution.maxCompletedExecutions",
                capture: crate::protocol::ExecutionOutputCapture::None,
                retain_events: false,
                event_limit: 1,
                event_bytes_limit: 1,
                uses_pty: false,
                value_kind: crate::state::ExecutionValueKind::None,
                semantic_result_path: None,
                pending_outcome: None,
                deadline_ms: None,
                expires_at_ms,
                deadline_task: None,
                resident_process_id: Some(process_id.to_owned()),
            },
        );
        vm.execution_processes
            .insert(process_id.to_owned(), execution_id.to_owned());
        drop(vm);
        sidecar
            .bind_extension_process_resource(
                cleanup_test_ownership(vm_id),
                String::from("test.cleanup"),
                extension_session_id.clone(),
                process_id.to_owned(),
            )
            .expect("bind resident process to extension session");
        extension_session_id
    }

    async fn execute_cleanup_request(
        sidecar: &mut NativeSidecar<LocalBridge>,
        request_id: RequestId,
        vm_id: &str,
        payload: RequestPayload,
    ) -> CompletedRequest {
        sidecar
            .prepare_request_wire(wire_request(RequestFrame::new(
                request_id,
                cleanup_test_ownership(vm_id),
                payload,
            )))
            .expect("prepare cleanup request")
            .expect("cleanup request is detachable")
            .execute()
            .await
    }

    fn complete_cleanup_request(
        sidecar: &mut NativeSidecar<LocalBridge>,
        completed: CompletedRequest,
    ) -> ResponseFrame {
        compat_response(
            sidecar
                .complete_request(completed)
                .expect("complete cleanup request"),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retention_expiry_completion_prunes_extension_resident_process() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        let process_id = "retention-expired-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            "retention-expired-execution",
            process_id,
            false,
            true,
            Some(0),
        );

        let completed = execute_cleanup_request(
            &mut sidecar,
            301,
            &vm_id,
            RequestPayload::ListExecutions(crate::protocol::ListExecutionsRequest {}),
        )
        .await;
        assert!(extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
        complete_cleanup_request(&mut sidecar, completed);
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retention_eviction_completion_prunes_extension_resident_process() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        sidecar
            .vms
            .get_mut(&vm_id)
            .expect("cleanup test VM")
            .limits
            .execution
            .max_completed_executions = 0;
        let process_id = "retention-evicted-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            "retention-evicted-execution",
            process_id,
            false,
            true,
            None,
        );

        let completed = execute_cleanup_request(
            &mut sidecar,
            302,
            &vm_id,
            RequestPayload::ListExecutions(crate::protocol::ListExecutionsRequest {}),
        )
        .await;
        assert!(extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
        complete_cleanup_request(&mut sidecar, completed);
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reset_execution_completion_prunes_extension_resident_process() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        let execution_id = "reset-context";
        let process_id = "reset-resident-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            execution_id,
            process_id,
            true,
            false,
            None,
        );

        let completed = execute_cleanup_request(
            &mut sidecar,
            303,
            &vm_id,
            RequestPayload::ResetExecution(crate::protocol::ResetExecutionRequest {
                execution_id: execution_id.to_owned(),
            }),
        )
        .await;
        assert!(extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
        complete_cleanup_request(&mut sidecar, completed);
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn delete_execution_completion_prunes_extension_resident_process() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        let execution_id = "delete-context";
        let process_id = "delete-resident-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            execution_id,
            process_id,
            true,
            false,
            None,
        );

        let completed = execute_cleanup_request(
            &mut sidecar,
            304,
            &vm_id,
            RequestPayload::DeleteExecution(crate::protocol::DeleteExecutionRequest {
                execution_id: execution_id.to_owned(),
            }),
        )
        .await;
        assert!(extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
        complete_cleanup_request(&mut sidecar, completed);
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reused_resident_start_failure_completion_prunes_extension_process() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        let execution_id = "failed-reuse-context";
        let process_id = "failed-reuse-resident-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            execution_id,
            process_id,
            true,
            false,
            None,
        );

        let completed = execute_cleanup_request(
            &mut sidecar,
            305,
            &vm_id,
            RequestPayload::JavaScriptExecution(crate::protocol::JavaScriptExecutionRequest {
                process: crate::protocol::ProcessExecutionOptions {
                    identity: crate::protocol::ExecutionIdentityOptions {
                        context_id: Some(execution_id.to_owned()),
                    },
                    output: crate::protocol::ExecutionOutputOptions {
                        capture: None,
                        retain_events: None,
                    },
                    operation_id: None,
                    background: None,
                    cwd: None,
                    env: None,
                    args: Vec::new(),
                    stdin: None,
                    timeout_ms: None,
                    pty: None,
                },
                source: String::from("globalThis.unreachable = true"),
                format: Some(crate::protocol::JavaScriptModuleFormat::CommonJs),
                file_path: None,
                inputs: None,
            }),
        )
        .await;
        assert!(extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
        complete_cleanup_request(&mut sidecar, completed);
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_completion_prunes_extension_process_effect_even_on_error() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1).await.remove(0);
        let process_id = "failed-request-effect-process";
        let extension_session_id = insert_bound_resident_cleanup_fixture(
            &mut sidecar,
            &vm_id,
            "failed-request-effect-execution",
            process_id,
            true,
            false,
            None,
        );
        let request = RequestFrame::new(
            306,
            cleanup_test_ownership(&vm_id),
            RequestPayload::ListExecutions(crate::protocol::ListExecutionsRequest {}),
        );
        let effects = sidecar.request_completion_effects(&request);
        effects
            .record_exited_process(process_id)
            .expect("record exited process effect");
        let prepared = PreparedRequest::from_future_with_effects(
            request,
            async {
                Err(SidecarError::InvalidState(String::from(
                    "failure after process cleanup",
                )))
            },
            effects,
        );

        let completed = prepared.execute().await;
        assert!(completed.failed());
        let response = complete_cleanup_request(&mut sidecar, completed);
        assert!(matches!(response.payload, ResponsePayload::Rejected(_)));
        assert!(!extension_tracks_process(
            &sidecar,
            &extension_session_id,
            process_id
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_prepared_request_waits_without_borrowing_coordinator_and_preserves_identity() {
        fn assert_owned<T: 'static>() {}
        assert_owned::<PreparedRequest>();

        let mut sidecar = test_sidecar();
        let request = RequestFrame::new(
            41,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            RequestPayload::GetZombieTimerCount(
                crate::protocol::GetZombieTimerCountRequest::default(),
            ),
        );
        let response = agentos_native_sidecar_core::zombie_timer_count_response(&request, 7);
        let (release, wait) = tokio::sync::oneshot::channel();
        let prepared = PreparedRequest::from_future(request.clone(), async move {
            wait.await
                .map_err(|_| SidecarError::Execution(String::from("test gate dropped")))?;
            Ok(DispatchResult {
                response,
                events: Vec::new(),
            })
        });

        let mut task = Box::pin(prepared.execute());
        assert!(
            poll_future_once(task.as_mut()).is_none(),
            "the owned operation should remain independently pending"
        );
        release.send(()).expect("release prepared request");
        let completed = task.await;
        assert_eq!(completed.request.request_id, request.request_id);
        assert_eq!(completed.request.ownership, request.ownership);
        assert!(!completed.failed());

        let response = compat_response(
            sidecar
                .complete_request(completed)
                .expect("complete prepared request"),
        );
        assert_eq!(response.request_id, request.request_id);
        assert_eq!(response.ownership, request.ownership);
        assert!(matches!(
            response.payload,
            ResponsePayload::ZombieTimerCount(crate::protocol::ZombieTimerCountResponse {
                count: 7
            })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn gated_vm_a_operation_does_not_delay_vm_b_command() {
        let mut sidecar = test_sidecar();
        let vm_ids = create_test_vms(&mut sidecar, 2).await;
        let vm_a = sidecar.vms.handle(&vm_ids[0]).expect("VM A handle");
        let vm_b = sidecar.vms.handle(&vm_ids[1]).expect("VM B handle");
        let request = RequestFrame::new(
            101,
            OwnershipScope::vm(
                "vm-handle-test-connection",
                "vm-handle-test-session",
                vm_ids[0].clone(),
            ),
            RequestPayload::GetZombieTimerCount(
                crate::protocol::GetZombieTimerCountRequest::default(),
            ),
        );
        let response = agentos_native_sidecar_core::zombie_timer_count_response(&request, 0);
        let (release, wait) = tokio::sync::oneshot::channel();
        let prepared = PreparedRequest::from_future(request, async move {
            wait.await
                .map_err(|_| SidecarError::Execution(String::from("test gate dropped")))?;
            vm_a.try_command("gated VM A test command", |vm| {
                vm.attached_child_event_cursor = 1;
                Ok(())
            })?;
            Ok(DispatchResult {
                response,
                events: Vec::new(),
            })
        });
        let mut vm_a_operation = Box::pin(prepared.execute());

        assert!(
            poll_future_once(vm_a_operation.as_mut()).is_none(),
            "VM A operation must be waiting at the deterministic gate"
        );
        vm_b.try_command("independent VM B test command", |vm| {
            vm.attached_child_event_cursor = 7;
            Ok(())
        })
        .expect("VM B command completes while VM A is gated");
        assert_eq!(
            vm_b.try_read("verify VM B command", |vm| vm.attached_child_event_cursor)
                .expect("read VM B"),
            7
        );

        release.send(()).expect("release VM A gate");
        let completed = vm_a_operation.await;
        assert!(!completed.failed());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vm_execution_engines_are_partitioned_by_vm() {
        let mut sidecar = test_sidecar();
        let vm_ids = create_test_vms(&mut sidecar, 2).await;
        let vm_a = sidecar.vms.handle(&vm_ids[0]).expect("VM A handle");
        let vm_b = sidecar.vms.handle(&vm_ids[1]).expect("VM B handle");
        let engines_a = vm_a
            .try_read("clone VM A engines", |vm| vm.execution_engines.clone())
            .expect("VM A engines");
        let engines_b = vm_b
            .try_read("clone VM B engines", |vm| vm.execution_engines.clone())
            .expect("VM B engines");

        let _held_a = engines_a
            .javascript("gate VM A JavaScript engine")
            .expect("borrow VM A JavaScript engine");
        vm_a.try_command("mutate VM A state outside its held engine", |vm| {
            vm.attached_child_event_cursor = 11;
            Ok(())
        })
        .expect("VM A state remains accessible while its engine is held");
        assert_eq!(
            vm_a.try_read("verify VM A state outside its held engine", |vm| {
                vm.attached_child_event_cursor
            })
            .expect("read VM A state while its engine is held"),
            11
        );
        let _independent_b = engines_b
            .javascript("enter VM B JavaScript engine")
            .expect("VM B engine remains independently available");
        let conflict = engines_a
            .javascript("re-enter VM A JavaScript engine")
            .expect_err("same-VM engine re-entry must be a typed conflict");
        assert!(conflict
            .to_string()
            .contains("ERR_AGENTOS_VM_EXECUTION_CONFLICT"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn execute_and_language_routes_are_prepared_without_central_fallback() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let ownership = OwnershipScope::vm(
            "vm-handle-test-connection",
            "vm-handle-test-session",
            vm_id.clone(),
        );
        let execute = RequestFrame::new(
            111,
            ownership.clone(),
            RequestPayload::Execute(ExecuteRequest {
                process_id: String::from("prepared-execute"),
                command: Some(String::from("node")),
                runtime: None,
                entrypoint: None,
                args: vec![String::from("-e"), String::from("0")],
                env: Default::default(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        );
        let prepared_execute = sidecar
            .prepare_request_wire(wire_request(execute))
            .expect("prepare execute")
            .expect("Execute must not use coordinated fallback");
        drop(prepared_execute);

        let lifecycle = RequestFrame::new(
            112,
            ownership,
            RequestPayload::ListExecutions(crate::protocol::ListExecutionsRequest {}),
        );
        let prepared_lifecycle = sidecar
            .prepare_request_wire(wire_request(lifecycle))
            .expect("prepare language lifecycle")
            .expect("ExecutionLifecycle must not use coordinated fallback");
        drop(prepared_lifecycle);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn prepared_filesystem_command_avoids_central_fallback() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let request = RequestFrame::new(
            102,
            OwnershipScope::vm("vm-handle-test-connection", "vm-handle-test-session", vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: crate::protocol::GuestFilesystemOperation::Stat,
                path: String::from("/"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        );

        let prepared = sidecar
            .prepare_request_wire(wire_request(request))
            .expect("prepare filesystem command")
            .expect("filesystem command is detachable");
        drop(prepared);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invalid_owned_guest_calls_become_prepared_rejections() {
        let mut sidecar = test_sidecar();
        let filesystem = RequestFrame::new(
            103,
            OwnershipScope::connection("not-vm-owned"),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: crate::protocol::GuestFilesystemOperation::Stat,
                path: String::from("/"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        );
        let filesystem = sidecar
            .prepare_request_wire(wire_request(filesystem))
            .expect("invalid filesystem ownership is not a router failure")
            .expect("filesystem call is always prepared")
            .execute()
            .await;
        assert!(filesystem.failed());

        let kernel = RequestFrame::new(
            104,
            OwnershipScope::connection("not-vm-owned"),
            RequestPayload::GuestKernelCall(crate::protocol::GuestKernelCallRequest {
                execution_id: String::from("missing-execution"),
                operation: String::from("noop"),
                payload: Vec::new(),
            }),
        );
        let kernel = sidecar
            .prepare_request_wire(wire_request(kernel))
            .expect("invalid kernel ownership is not a router failure")
            .expect("kernel call is always prepared")
            .execute()
            .await;
        assert!(kernel.failed());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn overlapping_vm_commands_return_typed_conflict_without_panicking() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let outer = sidecar.vms.handle(&vm_id).expect("outer VM handle");
        let nested = outer.clone();

        let error = outer
            .try_command("outer test command", |_| {
                nested.try_command("nested test command", |_| Ok(()))
            })
            .expect_err("nested command must report a typed conflict");
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_VM_COORDINATOR_CONFLICT"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn live_vm_handle_makes_disposal_conflict_typed_and_non_destructive() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let live_handle = sidecar.vms.handle(&vm_id).expect("live VM handle");

        let error = match sidecar.vms.try_remove(&vm_id, "dispose") {
            Ok(_) => panic!("live handle must exclude disposal"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("ERR_AGENTOS_VM_COORDINATOR_CONFLICT"));
        assert!(sidecar.vms.contains_key(&vm_id));

        drop(live_handle);
        assert!(sidecar
            .vms
            .try_remove(&vm_id, "dispose")
            .expect("removal after handle release")
            .is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preparation_keeps_cross_connection_ownership_rejection_terminal() {
        let mut sidecar = test_sidecar();
        sidecar.connections.insert(
            String::from("owner"),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([String::from("session-1")]),
            },
        );
        sidecar.connections.insert(
            String::from("attacker"),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::new(),
            },
        );
        sidecar.sessions.insert(
            String::from("session-1"),
            SessionState {
                connection_id: String::from("owner"),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids: BTreeSet::from([String::from("vm-1")]),
            },
        );
        let request = RequestFrame::new(
            77,
            OwnershipScope::vm("attacker", "session-1", "vm-1"),
            RequestPayload::ProvidedCommands(crate::protocol::ProvidedCommandsRequest {}),
        );

        let prepared = sidecar
            .prepare_request_wire(wire_request(request.clone()))
            .expect("prepare query route")
            .expect("query route is detachable");
        let completed = prepared.execute().await;
        assert!(completed.failed());
        let response = compat_response(
            sidecar
                .complete_request(completed)
                .expect("ownership error becomes a canonical response"),
        );
        assert_eq!(response.request_id, request.request_id);
        assert_eq!(response.ownership, request.ownership);
        match response.payload {
            ResponsePayload::Rejected(rejected) => {
                assert!(rejected
                    .message
                    .contains("session session-1 is not owned by connection attacker"));
            }
            payload => panic!("expected ownership rejection, got {payload:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn short_coordinator_routes_and_unknown_extension_never_fall_back() {
        let mut sidecar = test_sidecar();
        let invalid_authenticate = RequestFrame::new(
            200,
            OwnershipScope::connection("client-hint"),
            RequestPayload::Authenticate(crate::protocol::AuthenticateRequest {
                client_name: String::from("invalid-prepared-route-test"),
                auth_token: String::new(),
                protocol_version: crate::protocol::PROTOCOL_VERSION + 1,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        );
        let invalid_authenticate = sidecar
            .prepare_request_wire(wire_request(invalid_authenticate))
            .expect("invalid authenticate is a prepared rejection")
            .expect("authenticate is always prepared")
            .execute()
            .await;
        assert!(invalid_authenticate.failed());

        let unauthenticated_open = RequestFrame::new(
            201,
            OwnershipScope::connection("unknown-connection"),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: std::collections::HashMap::new(),
            }),
        );
        let unauthenticated_open = sidecar
            .prepare_request_wire(wire_request(unauthenticated_open))
            .expect("unauthenticated open is a prepared rejection")
            .expect("open session is always prepared")
            .execute()
            .await;
        assert!(unauthenticated_open.failed());

        let authenticate = RequestFrame::new(
            202,
            OwnershipScope::connection("client-hint"),
            RequestPayload::Authenticate(crate::protocol::AuthenticateRequest {
                client_name: String::from("prepared-route-test"),
                auth_token: String::new(),
                protocol_version: crate::protocol::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        );
        let authenticated = sidecar
            .prepare_request_wire(wire_request(authenticate))
            .expect("prepare authenticate")
            .expect("authenticate is always prepared");
        assert!(sidecar.connections.is_empty());
        assert_eq!(sidecar.next_connection_id, 0);
        let authenticated = authenticated.execute().await;
        assert!(
            sidecar.connections.is_empty(),
            "executing the owned response must not mutate central membership before completion"
        );
        let authenticated = compat_response(
            sidecar
                .complete_request(authenticated)
                .expect("complete authenticate"),
        );
        let ResponsePayload::Authenticated(authenticated) = authenticated.payload else {
            panic!("authenticate returned a different response");
        };
        assert!(sidecar
            .connections
            .contains_key(&authenticated.connection_id));

        let open_session = RequestFrame::new(
            203,
            OwnershipScope::connection(authenticated.connection_id.clone()),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: std::collections::HashMap::new(),
            }),
        );
        let opened = sidecar
            .prepare_request_wire(wire_request(open_session))
            .expect("prepare open session")
            .expect("open session is always prepared");
        assert!(sidecar.sessions.is_empty());
        assert_eq!(sidecar.next_session_id, 0);
        let opened = opened.execute().await;
        assert!(
            sidecar.sessions.is_empty(),
            "owned response execution stages central session membership until completion"
        );
        let opened = compat_response(
            sidecar
                .complete_request(opened)
                .expect("complete open session"),
        );
        assert!(matches!(opened.payload, ResponsePayload::SessionOpened(_)));

        let unknown_extension = RequestFrame::new(
            204,
            OwnershipScope::connection(authenticated.connection_id),
            RequestPayload::Ext(ExtEnvelope {
                namespace: String::from("missing.prepared.extension"),
                payload: Vec::new(),
            }),
        );
        let rejected = sidecar
            .prepare_request_wire(wire_request(unknown_extension))
            .expect("prepare unknown extension")
            .expect("unknown extension has a prepared canonical rejection")
            .execute()
            .await;
        let rejected = compat_response(
            sidecar
                .complete_request(rejected)
                .expect("complete unknown extension rejection"),
        );
        assert!(matches!(
            rejected.payload,
            ResponsePayload::Rejected(RejectedResponse { ref code, .. }) if code == "unknown_extension"
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn host_callback_registration_and_snapshot_queries_never_fall_back() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let ownership = OwnershipScope::vm(
            "vm-handle-test-connection",
            "vm-handle-test-session",
            &vm_id,
        );
        let registration = RequestFrame::new(
            204,
            ownership.clone(),
            RequestPayload::RegisterHostCallbacks(crate::protocol::RegisterHostCallbacksRequest {
                name: String::from("prepared-bindings"),
                description: String::from("prepared route test bindings"),
                command_aliases: vec![String::from("agentos-prepared-bindings")],
                registry_command_aliases: vec![String::from("agentos")],
                callbacks: std::collections::HashMap::from([(
                    String::from("call"),
                    crate::protocol::RegisteredHostCallbackDefinition {
                        description: String::from("prepared route test callback"),
                        input_schema: String::from(
                            r#"{"type":"object","additionalProperties":false}"#,
                        ),
                        timeout_ms: None,
                        examples: Vec::new(),
                    },
                )]),
            }),
        );
        let prepared_registration = sidecar
            .prepare_request_wire(wire_request(registration))
            .expect("prepare host callback registration")
            .expect("host callback registration is prepared");
        assert!(
            !sidecar
                .vms
                .get(&vm_id)
                .expect("test VM")
                .bindings
                .contains_key("prepared-bindings"),
            "preparing host callback registration must not mutate VM state"
        );
        let completed_registration = prepared_registration.execute().await;
        assert!(
            sidecar
                .vms
                .get(&vm_id)
                .expect("test VM")
                .bindings
                .contains_key("prepared-bindings"),
            "owned host callback registration runs only when its prepared future executes"
        );
        sidecar
            .complete_request(completed_registration)
            .expect("complete host callback registration");

        let queries = [
            RequestPayload::GetProcessSnapshot(crate::protocol::GetProcessSnapshotRequest {}),
            RequestPayload::GetResourceSnapshot(crate::protocol::GetResourceSnapshotRequest {}),
            RequestPayload::GetZombieTimerCount(
                crate::protocol::GetZombieTimerCountRequest::default(),
            ),
            RequestPayload::ProvidedCommands(crate::protocol::ProvidedCommandsRequest {}),
            RequestPayload::ListMounts(crate::protocol::ListMountsRequest {}),
        ];
        for (index, payload) in queries.into_iter().enumerate() {
            let request = RequestFrame::new(210 + index as RequestId, ownership.clone(), payload);
            assert!(sidecar
                .prepare_request_wire(wire_request(request))
                .expect("prepare snapshot/list query")
                .is_some());
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owned_host_callback_restore_failure_rolls_back_registry_and_permissions() {
        let mut sidecar = test_sidecar();
        let vm_id = create_test_vms(&mut sidecar, 1)
            .await
            .pop()
            .expect("test VM id");
        let (original_permissions, original_bindings, original_paths) = {
            let vm = sidecar.vms.get(&vm_id).expect("test VM");
            (
                vm.configuration.permissions.clone(),
                vm.bindings.clone(),
                vm.command_guest_paths.clone(),
            )
        };
        sidecar
            .bridge
            .queue_set_vm_permissions_result(Ok(()))
            .expect("queue temporary allow-all permission set");
        sidecar
            .bridge
            .queue_set_vm_permissions_result(Err(SidecarError::Bridge(String::from(
                "injected owned registration restore failure",
            ))))
            .expect("queue original permission restore failure");

        let request = RequestFrame::new(
            230,
            cleanup_test_ownership(&vm_id),
            RequestPayload::RegisterHostCallbacks(crate::protocol::RegisterHostCallbacksRequest {
                name: String::from("rollback-bindings"),
                description: String::from("rollback binding collection"),
                command_aliases: vec![String::from("rollback-command")],
                registry_command_aliases: vec![String::from("agentos")],
                callbacks: std::collections::HashMap::from([(
                    String::from("call"),
                    crate::protocol::RegisteredHostCallbackDefinition {
                        description: String::from("rollback callback"),
                        input_schema: String::from(
                            r#"{"type":"object","additionalProperties":false}"#,
                        ),
                        timeout_ms: None,
                        examples: Vec::new(),
                    },
                )]),
            }),
        );
        let completed = sidecar
            .prepare_request_wire(wire_request(request))
            .expect("prepare callback registration")
            .expect("callback registration is detachable")
            .execute()
            .await;
        assert!(completed.failed());
        let response = compat_response(
            sidecar
                .complete_request(completed)
                .expect("complete failed callback registration"),
        );
        let ResponsePayload::Rejected(rejected) = response.payload else {
            panic!("expected callback registration rejection");
        };
        assert!(rejected
            .message
            .contains("injected owned registration restore failure"));

        let vm = sidecar.vms.get(&vm_id).expect("test VM");
        assert_eq!(vm.bindings, original_bindings);
        assert_eq!(vm.command_guest_paths, original_paths);
        assert!(!vm.kernel.commands().contains_key("rollback-command"));
        drop(vm);
        assert_eq!(
            sidecar
                .bridge
                .permissions
                .lock()
                .expect("read bridge permissions")
                .get(&vm_id),
            Some(&original_permissions),
        );
    }

    #[test]
    fn mutating_vm_request_uses_detached_vm_handle_path() {
        let mut sidecar = test_sidecar();
        let request = RequestFrame::new(
            91,
            OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            RequestPayload::WriteStdin(WriteStdinRequest {
                process_id: String::from("process-1"),
                chunk: vec![1, 2, 3],
            }),
        );
        assert!(
            sidecar
                .prepare_request_wire(wire_request(request))
                .expect("classify mutating request")
                .is_some(),
            "mutating VM state must use the detached VM-handle path"
        );
    }
}

#[cfg(test)]
mod legacy_child_spawn_options_tests {
    use super::*;

    #[test]
    fn legacy_v8_string_bridge_preserves_canonical_spawn_options() {
        let vm_guest_env = BTreeMap::from([
            (
                String::from("AGENTOS_ALLOWED_NODE_BUILTINS"),
                String::from("node:path"),
            ),
            (String::from("AGENTOS_NOT_ALLOWED"), String::from("drop-me")),
        ]);
        let parsed = parse_legacy_javascript_child_process_spawn_options(
            &vm_guest_env,
            r#"{
                "argv0":"custom-zero",
                "cloexecFds":[9,10],
                "localReplacement":true,
                "executableFd":11,
                "cwd":"/work",
                "env":{"VISIBLE":"yes"},
                "internalBootstrapEnv":{
                    "AGENTOS_WASM_INITIAL_SIGNAL_MASK":"[10]",
                    "AGENTOS_NOT_ALLOWED":"drop-me-too"
                },
                "spawnAttrFlags":70,
                "spawnExactPath":false,
                "spawnSearchPath":"/custom/bin:/bin",
                "spawnSchedPolicy":0,
                "spawnSchedPriority":0,
                "spawnPgroup":42,
                "spawnSignalDefaults":[13],
                "spawnSignalMask":[10,12],
                "spawnFileActions":[{
                    "command":2,
                    "guestFd":41,
                    "fd":8,
                    "sourceFd":7,
                    "guestSourceFd":40,
                    "oflag":0,
                    "mode":420,
                    "path":"/tmp/unused"
                }],
                "spawnFdMappings":[[40,7],[50,8]],
                "input":{"type":"Buffer","data":[97]},
                "shell":true,
                "detached":true,
                "stdio":["pipe","inherit","ignore"],
                "maxBuffer":1234,
                "timeout":5678,
                "killSignal":"SIGUSR2"
            }"#,
        )
        .expect("parse V8 three-string options payload");

        assert_eq!(parsed.max_buffer, Some(1234));
        let options = parsed.options;
        assert_eq!(options.argv0.as_deref(), Some("custom-zero"));
        assert_eq!(options.cloexec_fds, vec![9, 10]);
        assert!(options.local_replacement);
        assert_eq!(options.executable_fd, Some(11));
        assert_eq!(options.cwd.as_deref(), Some("/work"));
        assert_eq!(options.env.get("VISIBLE").map(String::as_str), Some("yes"));
        assert_eq!(
            options
                .internal_bootstrap_env
                .get("AGENTOS_ALLOWED_NODE_BUILTINS")
                .map(String::as_str),
            Some("node:path")
        );
        assert_eq!(
            options
                .internal_bootstrap_env
                .get("AGENTOS_WASM_INITIAL_SIGNAL_MASK")
                .map(String::as_str),
            Some("[10]")
        );
        assert!(!options
            .internal_bootstrap_env
            .contains_key("AGENTOS_NOT_ALLOWED"));
        assert_eq!(options.spawn_attr_flags, 70);
        assert!(!options.spawn_exact_path);
        assert_eq!(
            options.spawn_search_path.as_deref(),
            Some("/custom/bin:/bin")
        );
        assert_eq!(options.spawn_sched_policy, Some(0));
        assert_eq!(options.spawn_sched_priority, Some(0));
        assert_eq!(options.spawn_pgroup, Some(42));
        assert_eq!(options.spawn_signal_defaults, vec![13]);
        assert_eq!(options.spawn_signal_mask, vec![10, 12]);
        assert_eq!(options.spawn_fd_mappings, vec![[40, 7], [50, 8]]);
        assert_eq!(options.spawn_file_actions.len(), 1);
        let action = &options.spawn_file_actions[0];
        assert_eq!(action.command, 2);
        assert_eq!(action.guest_fd, Some(41));
        assert_eq!(action.fd, 8);
        assert_eq!(action.source_fd, 7);
        assert_eq!(action.guest_source_fd, Some(40));
        assert_eq!(action.oflag, 0);
        assert_eq!(action.mode, 420);
        assert_eq!(action.path, "/tmp/unused");
        assert_eq!(options.input, Some(json!({"type":"Buffer","data":[97]})));
        assert!(options.shell);
        assert!(options.detached);
        assert_eq!(options.stdio, vec!["pipe", "inherit", "ignore"]);
        assert_eq!(options.timeout, Some(5678));
        assert_eq!(options.kill_signal.as_deref(), Some("SIGUSR2"));
    }
}

#[cfg(test)]
mod symlinked_node_modules_hint_tests {
    use super::symlinked_node_modules_hint;

    // Positive cases: each non-flat package manager's store/PnP signature.
    #[test]
    fn matches_pnpm_store_enoent() {
        // Real pi-coding-agent failure: getPackageDir() falls back to a
        // dist/package.json inside the unreachable .pnpm store.
        let stderr = "Error: ENOENT: no such file or directory, open '/root/node_modules/.pnpm/@mariozechner+pi-coding-agent@0.60.0_x/node_modules/@mariozechner/pi-coding-agent/dist/package.json'";
        let hint = symlinked_node_modules_hint(stderr).expect("expected hoisted guidance");
        assert!(hint.contains("agentos can't load mounted node_modules"));
        assert!(!hint.contains("/root/node_modules/.pnpm/"));
    }

    #[test]
    fn matches_bun_store_enoent() {
        let stderr = "Error: ENOENT: no such file or directory, open '/root/node_modules/.bun/is-odd@3.0.1/node_modules/is-odd/package.json'";
        assert!(symlinked_node_modules_hint(stderr).is_some());
    }

    #[test]
    fn matches_yarn_pnpm_store_enoent() {
        let stderr = "Error: ENOENT: no such file or directory, open '/root/node_modules/.store/is-odd-npm-3.0.1-93c3c3f41b/package/package.json'";
        assert!(symlinked_node_modules_hint(stderr).is_some());
    }

    #[test]
    fn matches_pnp_declared_error() {
        // Yarn PnP's distinctive resolver error (no node_modules at all).
        let stderr = "Error: Your application tried to access is-number, but it isn't declared in your dependencies; this makes the require call ambiguous and unsound.";
        assert!(symlinked_node_modules_hint(stderr).is_some());
    }

    #[test]
    fn matches_pnp_cjs_module_not_found() {
        let stderr = "Error: Cannot find module 'is-odd'\n    at /root/.pnp.cjs:12345:18\n    code: 'MODULE_NOT_FOUND'";
        assert!(symlinked_node_modules_hint(stderr).is_some());
    }

    #[test]
    fn matches_virtual_instance() {
        let stderr = "Error: ENOENT: no such file or directory, open '/root/.yarn/__virtual__/is-odd-abc/1/node_modules/is-odd/package.json'";
        assert!(symlinked_node_modules_hint(stderr).is_some());
    }

    // Negative cases: must not fire.
    #[test]
    fn ignores_enoent_outside_a_store() {
        let stderr = "Error: ENOENT: no such file or directory, open '/tmp/scratch/config.json'";
        assert!(symlinked_node_modules_hint(stderr).is_none());
    }

    #[test]
    fn ignores_store_path_without_missing_file() {
        let stderr =
            "loaded /root/node_modules/.pnpm/some-pkg@1.0.0/node_modules/some-pkg/index.js";
        assert!(symlinked_node_modules_hint(stderr).is_none());
    }

    #[test]
    fn ignores_flat_node_modules_enoent() {
        // npm / yarn-nm / pnpm-hoisted: flat, no store dir in the path.
        let stderr = "Error: ENOENT: no such file or directory, open '/root/node_modules/is-odd/missing-asset.json'";
        assert!(symlinked_node_modules_hint(stderr).is_none());
    }

    #[test]
    fn ignores_unrelated_failure() {
        let stderr = "Error: connect ECONNREFUSED 127.0.0.1:443";
        assert!(symlinked_node_modules_hint(stderr).is_none());
    }
}

#[cfg(test)]
mod structured_event_frame_tests {
    use super::*;

    #[test]
    fn structured_event_frame_round_trips_limit_warning() {
        let mut detail = std::collections::HashMap::new();
        // Pin a real emitted limit name rather than a fictional string.
        let limit_name = TrackedLimit::JavascriptEventChannel.as_str();
        detail.insert(String::from("limit"), String::from(limit_name));
        detail.insert(String::from("fillPercent"), String::from("82"));

        let wire = structured_event_frame("conn-1", "limit_warning", detail)
            .expect("build structured event frame");
        let compat = crate::wire::event_frame_to_compat(wire).expect("convert to compat");

        match compat.payload {
            EventPayload::Structured(event) => {
                assert_eq!(event.name, "limit_warning");
                assert_eq!(
                    event.detail.get("limit").map(String::as_str),
                    Some(limit_name)
                );
                assert_eq!(
                    event.detail.get("fillPercent").map(String::as_str),
                    Some("82")
                );
            }
            other => panic!("expected structured payload, got {other:?}"),
        }
        match compat.ownership {
            OwnershipScope::ConnectionOwnership(inner) => {
                assert_eq!(inner.connection_id, "conn-1");
            }
            other => panic!("expected connection ownership, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod guest_limit_diagnostic_tests {
    use super::guest_limit_diagnostic;
    use agentos_runtime::accounting::{LimitError, ResourceClass};

    fn limit(scope: &str, used: usize) -> LimitError {
        LimitError {
            scope: scope.to_owned(),
            resource: ResourceClass::AsyncCompletions,
            used,
            requested: 1,
            limit: 8,
            config_path: String::from("runtime.resources.maxAsyncCompletions"),
        }
    }

    #[test]
    fn vm_limit_reports_only_the_requesting_vm_usage() {
        let diagnostic = guest_limit_diagnostic(&limit("vm=vm-1 generation=7", 6));
        assert_eq!(diagnostic.scope, "vm");
        assert_eq!(diagnostic.current_usage, Some(6));
        assert!(diagnostic.message.contains("used=6"));
    }

    #[test]
    fn process_limit_hides_cross_vm_aggregate_usage() {
        let diagnostic = guest_limit_diagnostic(&limit("sidecar-process", 7));
        assert_eq!(diagnostic.scope, "process");
        assert_eq!(diagnostic.current_usage, None);
        assert!(!diagnostic.message.contains("used=7"));
        assert!(diagnostic.message.contains("requested=1 limit=8"));
        assert!(diagnostic
            .message
            .contains("runtime.resources.maxAsyncCompletions"));
    }
}

#[cfg(test)]
mod dispose_lifecycle_tests {
    use super::*;
    use crate::extension::ExtensionResponse;
    use crate::stdio::LocalBridge;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("dispose lifecycle test runtime")
            .block_on(future)
    }

    fn test_sidecar() -> NativeSidecar<LocalBridge> {
        NativeSidecar::new(LocalBridge::default()).expect("build test sidecar")
    }

    // Register a connection + session directly so the dispose paths can be
    // exercised without spinning up a V8-backed VM.
    fn insert_session(
        sidecar: &mut NativeSidecar<LocalBridge>,
        connection_id: &str,
        session_id: &str,
        vm_ids: BTreeSet<String>,
    ) {
        sidecar.connections.insert(
            connection_id.to_string(),
            ConnectionState {
                auth_token: String::new(),
                sessions: BTreeSet::from([session_id.to_string()]),
            },
        );
        sidecar.sessions.insert(
            session_id.to_string(),
            SessionState {
                connection_id: connection_id.to_string(),
                placement: crate::protocol::SidecarPlacement::SidecarPlacementShared(
                    crate::protocol::SidecarPlacementShared { pool: None },
                ),
                metadata: BTreeMap::new(),
                vm_ids,
            },
        );
    }

    struct RecordingExtension {
        namespace: String,
        session_disposed: Arc<AtomicUsize>,
    }

    impl Extension for RecordingExtension {
        fn namespace(&self) -> &str {
            &self.namespace
        }

        fn handle_request<'a>(
            &'a self,
            _ctx: ExtensionContext,
            _payload: Vec<u8>,
        ) -> ExtensionFuture<'a, ExtensionResponse> {
            Box::pin(async { Ok(ExtensionResponse::new(Vec::new())) })
        }

        fn on_session_disposed<'a>(&'a self, _ctx: ExtensionSnapshot) -> ExtensionFuture<'a, ()> {
            let counter = self.session_disposed.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    fn register_recording_extension(sidecar: &mut NativeSidecar<LocalBridge>) -> Arc<AtomicUsize> {
        let counter = Arc::new(AtomicUsize::new(0));
        sidecar
            .register_extension(Box::new(RecordingExtension {
                namespace: String::from("dev.test.dispose"),
                session_disposed: counter.clone(),
            }))
            .expect("register recording extension");
        counter
    }

    // H4: the extension per-session teardown hook fires on ConnectionClosed so an
    // ACP-style extension can release per-session state on client disconnect.
    #[test]
    fn connection_closed_dispose_invokes_extension_session_teardown() {
        let mut sidecar = test_sidecar();
        let counter = register_recording_extension(&mut sidecar);
        insert_session(&mut sidecar, "conn-1", "session-1", BTreeSet::new());

        block_on(sidecar.dispose_session("conn-1", "session-1", DisposeReason::ConnectionClosed))
            .expect("dispose session on connection close");

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "extension session-teardown hook must fire on ConnectionClosed"
        );
        assert!(
            !sidecar.sessions.contains_key("session-1"),
            "the disposed session must be reclaimed"
        );
    }

    // H4 (negative): a client-requested dispose is not a disconnect, so the
    // teardown hook must not fire.
    #[test]
    fn requested_dispose_does_not_invoke_extension_session_teardown() {
        let mut sidecar = test_sidecar();
        let counter = register_recording_extension(&mut sidecar);
        insert_session(&mut sidecar, "conn-1", "session-1", BTreeSet::new());

        block_on(sidecar.dispose_session("conn-1", "session-1", DisposeReason::Requested))
            .expect("dispose session on request");

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "the teardown hook is reserved for client disconnect"
        );
    }

    // M5: disposing a session records its scope for the stdio transport to drain.
    #[test]
    fn dispose_session_records_disposed_scope() {
        let mut sidecar = test_sidecar();
        insert_session(&mut sidecar, "conn-1", "session-1", BTreeSet::new());

        block_on(sidecar.dispose_session("conn-1", "session-1", DisposeReason::Requested))
            .expect("dispose session");

        assert_eq!(
            sidecar.take_disposed_sessions(),
            vec![(String::from("conn-1"), String::from("session-1"))],
            "dispose must publish the session scope so stdio can untrack it"
        );
    }

    // H1 + M6: every per-VM tracking map is reclaimed for a disposed VM. The
    // output-buffer map (M6) was previously only removed on a successful handoff,
    // and the engine/extension maps (H1) were only reclaimed after the fallible
    // teardown steps' `?`, so any failure stranded them.
    #[test]
    fn reclaim_vm_tracking_clears_every_per_vm_map() {
        let mut sidecar = test_sidecar();
        insert_session(
            &mut sidecar,
            "conn-1",
            "session-1",
            BTreeSet::from([String::from("vm-1")]),
        );
        sidecar.extension_process_output_buffers.insert(
            (String::from("vm-1"), String::from("proc-1")),
            ExtensionBufferedProcessOutput::default(),
        );
        sidecar.extension_sessions.insert(
            (String::from("ns"), String::from("ext-sess-1")),
            ExtensionSessionResources {
                ownership: OwnershipScope::vm("conn-1", "session-1", "vm-1"),
                process_ids: BTreeSet::new(),
                vm_ids: BTreeSet::from([String::from("vm-1")]),
            },
        );

        sidecar.reclaim_vm_tracking("session-1", "vm-1");

        assert!(
            sidecar.extension_process_output_buffers.is_empty(),
            "M6: the output-buffer map must be reclaimed on VM disposal"
        );
        assert!(
            sidecar.extension_sessions.is_empty(),
            "H1: an extension session bound only to the VM must be reclaimed"
        );
        assert!(
            !sidecar
                .sessions
                .get("session-1")
                .expect("session present")
                .vm_ids
                .contains("vm-1"),
            "the VM id must be removed from its session"
        );
    }

    // H1: a failing VM dispose inside the loop must not abandon the session. With
    // unregistered VM ids, `dispose_vm_internal` fails on `require_owned_vm`;
    // pre-fix the loop `?`-ed out and left the session in `self.sessions`.
    #[test]
    fn dispose_session_reclaims_session_even_when_a_vm_dispose_fails() {
        let mut sidecar = test_sidecar();
        insert_session(
            &mut sidecar,
            "conn-1",
            "session-1",
            BTreeSet::from([String::from("vm-a"), String::from("vm-b")]),
        );

        let result =
            block_on(sidecar.dispose_session("conn-1", "session-1", DisposeReason::Requested));

        assert!(
            result.is_err(),
            "a failing VM dispose must still surface an error"
        );
        assert!(
            !sidecar.sessions.contains_key("session-1"),
            "the session must be reclaimed even though VM dispose failed"
        );
    }
}
