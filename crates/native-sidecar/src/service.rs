use crate::bindings::register_host_callbacks;
use crate::bridge::{build_mount_plugin_registry, MountPluginContext};
pub(crate) use crate::execution::{
    apply_kernel_signal_registration, build_socket_path_context,
    deferred_kernel_wait_request_for_process, dispatch_loopback_http_request_deferred, error_code,
    flush_pending_kernel_stdin, format_tcp_resource, host_bytes_value, host_service_error_code,
    javascript_sync_rpc_arg_i32, javascript_sync_rpc_arg_str, javascript_sync_rpc_arg_u32,
    javascript_sync_rpc_arg_u32_optional, javascript_sync_rpc_arg_u64,
    javascript_sync_rpc_arg_u64_optional, javascript_sync_rpc_bytes_arg,
    javascript_sync_rpc_encoding, javascript_sync_rpc_may_make_fd_readable,
    javascript_sync_rpc_may_make_fd_writable, javascript_sync_rpc_option_bool,
    javascript_sync_rpc_option_u32, kernel_poll_response, kernel_stdin_read_response,
    mark_execute_exit_event_queued, parse_kernel_poll_args, parse_kernel_stdin_read_args,
    parse_signal, record_execute_exit_event_queue_wait, record_execute_phase,
    sanitize_javascript_child_process_internal_bootstrap_env,
    service_javascript_kernel_fd_write_sync_rpc, service_javascript_sync_rpc,
    settle_execution_host_call, HickoryDnsResolver, JavascriptSyncRpcServiceRequest,
    LoopbackHttpDispatchRequest,
};
use crate::extension::{
    Extension, ExtensionBufferedProcessOutput, ExtensionContext, ExtensionFuture, ExtensionHost,
    ExtensionSnapshot,
};
use crate::filesystem::guest_filesystem_call as filesystem_guest_filesystem_call;
use crate::limits::DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT;
use crate::protocol::{
    CloseStdinRequest, DisposeReason, EventFrame, EventPayload, ExecuteRequest, ExtEnvelope,
    GuestFilesystemCallRequest, GuestFilesystemResultResponse, KillProcessRequest,
    OpenSessionRequest, OwnershipScope, ProcessKilledResponse, ProcessStartedResponse,
    RejectedResponse, RequestFrame, RequestId, RequestPayload, ResponseFrame, ResponsePayload,
    SidecarRequestFrame, SidecarRequestPayload, SidecarResponseFrame, SidecarResponsePayload,
    SidecarResponseTracker, SidecarResponseTrackerError, StdinClosedResponse, StdinWrittenResponse,
    VmLifecycleState, WriteStdinRequest,
};
use crate::state::{
    ActiveExecutionEvent, BridgeError, ConnectionState, EventSinkTransport, ExecutionHostCall,
    ProcessEventEnvelope, QuarantinedVmGeneration, SessionState, SharedBridge, SharedEventSink,
    SharedSidecarRequestClient, SidecarRequestTransport, SocketFamily, SocketPathContext, VmState,
    EXECUTION_DRIVER_NAME,
};
use crate::NativeSidecarBridge;
use agentos_bridge::queue_tracker::{register_queue, QueueGauge, TrackedLimit};
use agentos_bridge::{
    CommandPermissionRequest, EnvironmentAccess, EnvironmentPermissionRequest, FilesystemAccess,
    FilesystemPermissionRequest, LifecycleEventRecord, LifecycleState, LogLevel, LogRecord,
    NetworkAccess, NetworkPermissionRequest, StructuredEventRecord,
};
use agentos_execution::backend::ExecutionBackendKind;
use agentos_execution::host::{ProcessLaunchOptions, ProcessLaunchRequest};
use agentos_execution::{
    record_sync_bridge_request_observed, JavascriptExecutionEngine, JavascriptExecutionError,
    PythonExecutionEngine, PythonExecutionError, WasmExecutionEngine, WasmExecutionError,
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
    authenticated_response as shared_authenticated_response, parse_process_signal_state_request,
    reject as shared_reject, respond as shared_respond, route_request_payload,
    session_opened_response, unsupported_host_callback_direction_dispatch,
    validate_authenticate_versions, vm_lifecycle_event as shared_vm_lifecycle_event,
    AuthenticateVersionError, RequestRoute,
};
use agentos_runtime::metrics::ResourceMetricClass;
use agentos_vm_config::{FsPermissionScope, PermissionMode, PermissionsPolicy};
// root_fs types moved to crate::vm
use agentos_kernel::vfs::VfsError;
use serde::Deserialize;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
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
    SidecarError::host("ERR_AGENTOS_PROCESS_EVENT_LIMIT", format!("process event queue exceeded {limit} pending events; raise runtime.protocol.maxProcessEvents"
    ))
}

fn sidecar_response_pending_overflow_error(limit: usize) -> SidecarError {
    SidecarError::host("ERR_AGENTOS_PENDING_RESPONSE_LIMIT", format!("sidecar response tracker exceeded {limit} pending responses; raise runtime.protocol.maxPendingResponses"
    ))
}

fn outbound_sidecar_request_queue_overflow_error(limit: usize) -> SidecarError {
    SidecarError::host("ERR_AGENTOS_OUTBOUND_REQUEST_LIMIT", format!("outbound sidecar request queue exceeded {limit} pending requests; raise runtime.protocol.maxOutboundRequests"
    ))
}

fn wire_protocol_error(error: crate::wire::ProtocolCodecError) -> SidecarError {
    SidecarError::InvalidState(format!("invalid generated wire protocol frame: {error}"))
}

fn wire_dispatch_result(
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

// SharedBridge struct and Clone impl moved to crate::state

#[derive(Debug, Default, Deserialize)]
struct LegacyJavascriptChildProcessSpawnOptions {
    // The V8 sync host binding still carries command/argv/options as three
    // strings. Flatten the canonical options object here so every newly added
    // field crosses that compatibility bridge automatically; keeping a second
    // hand-copied field list previously dropped POSIX spawn attributes and fd
    // mappings without an error.
    #[serde(flatten)]
    options: ProcessLaunchOptions,
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
) -> Result<(ProcessLaunchRequest, Option<usize>), SidecarError> {
    if let Some(value) = args.first().cloned() {
        if let Ok(request) = serde_json::from_value::<ProcessLaunchRequest>(value) {
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
        ProcessLaunchRequest {
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
            #[cfg(test)]
            emit_lifecycle_outcomes: Arc::new(Mutex::new(VecDeque::new())),
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
        #[cfg(test)]
        {
            let outcome = self
                .emit_lifecycle_outcomes
                .lock()
                .map_err(|_| {
                    SidecarError::Bridge(String::from(
                        "native sidecar test lifecycle outcome lock poisoned",
                    ))
                })?
                .pop_front();
            if let Some(Some(error)) = outcome {
                return Err(error);
            }
        }
        self.with_mut(|bridge| {
            bridge.emit_lifecycle(LifecycleEventRecord {
                vm_id: vm_id.to_owned(),
                state,
                detail: None,
            })
        })
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn queue_emit_lifecycle_result(
        &self,
        result: Result<(), SidecarError>,
    ) -> Result<(), SidecarError> {
        self.emit_lifecycle_outcomes
            .lock()
            .map_err(|_| {
                SidecarError::Bridge(String::from(
                    "native sidecar test lifecycle outcome lock poisoned",
                ))
            })?
            .push_back(result.err());
        Ok(())
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
            Some(reason) => format!("permission denied, {resource}: {reason}"),
            None => format!("permission denied, {resource}"),
        };
        Err(SidecarError::host("EACCES", message))
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
                Some(reason) => format!("permission denied, {resource}: {reason}"),
                None => format!("permission denied, {resource}"),
            };
            Err(SidecarError::host("EACCES", message))
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
        let stored = match self.permissions.lock() {
            Ok(stored) => stored,
            Err(_) => {
                eprintln!(
                    "ERR_AGENTOS_PERMISSION_STATE: permission policy lock is poisoned for VM {vm_id}; denying {capability} fail-closed"
                );
                return Some(PermissionDecision::deny(
                    "permission policy state is unavailable",
                ));
            }
        };
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

    // Stale queued events must still be drained through handle_process_event_envelope()
    // so the sidecar can emit the expected fail-closed log when teardown wins the race.
    let _ = sidecar;
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

// SocketPathContext, SocketFamily, VmListenPolicy moved to crate::state

impl SocketPathContext {
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
        family: SocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.tcp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn http_loopback_target(
        &self,
        family: SocketFamily,
        port: u16,
    ) -> Option<&crate::state::HttpLoopbackTarget> {
        self.http_loopback_targets.get(&(family, port))
    }

    pub(crate) fn translate_udp_loopback_port(
        &self,
        family: SocketFamily,
        port: u16,
    ) -> Option<u16> {
        self.udp_loopback_guest_to_host_ports
            .get(&(family, port))
            .copied()
    }

    pub(crate) fn guest_udp_port_for_host_port(
        &self,
        family: SocketFamily,
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
    pub(crate) javascript_engine: JavascriptExecutionEngine,
    pub(crate) python_engine: PythonExecutionEngine,
    pub(crate) wasm_engine: WasmExecutionEngine,
    pub(crate) next_connection_id: usize,
    pub(crate) next_session_id: usize,
    pub(crate) next_vm_id: usize,
    pub(crate) next_sidecar_request_id: RequestId,
    pub(crate) connections: BTreeMap<String, ConnectionState>,
    pub(crate) sessions: BTreeMap<String, SessionState>,
    pub(crate) vms: BTreeMap<String, VmState>,
    /// Detached generations whose asynchronous ownership has not reconciled.
    /// The combined active + quarantined generation count is admitted against
    /// `runtime.resources.maxCapabilities`, keeping this collection bounded.
    pub(crate) quarantined_vms: BTreeMap<u64, QuarantinedVmGeneration>,
    #[allow(dead_code)]
    pub(crate) process_event_sender: Sender<ProcessEventEnvelope>,
    pub(crate) process_event_receiver: Option<Receiver<ProcessEventEnvelope>>,
    pub(crate) process_event_notify: Arc<tokio::sync::Notify>,
    /// The single process-level deadline task that wakes cooperative kernel
    /// zombie reaping. It is replaced only when a genuinely earlier deadline
    /// appears; never one task or OS thread per process.
    pub(crate) kernel_reaper_task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) kernel_reaper_deadline: Option<Instant>,
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
        let dns_resolver: agentos_kernel::dns::SharedDnsResolver =
            Arc::new(HickoryDnsResolver::new(runtime_context.clone()));

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
        let mut javascript_engine = JavascriptExecutionEngine::new(runtime_context.clone());
        javascript_engine.set_event_notify(Some(Arc::clone(&process_event_notify)));
        let mut python_engine = PythonExecutionEngine::new(runtime_context.clone());
        python_engine.set_event_notify(Some(Arc::clone(&process_event_notify)));
        let mut wasm_engine = WasmExecutionEngine::new(runtime_context.clone());
        wasm_engine.set_event_notify(Some(Arc::clone(&process_event_notify)));

        Ok(Self {
            config,
            runtime_context: Some(runtime_context),
            dns_resolver,
            bridge,
            mount_plugins,
            cache_root,
            javascript_engine,
            python_engine,
            wasm_engine,
            next_connection_id: 0,
            next_session_id: 0,
            next_vm_id: 0,
            next_sidecar_request_id: -1,
            connections: BTreeMap::new(),
            sessions: BTreeMap::new(),
            vms: BTreeMap::new(),
            quarantined_vms: BTreeMap::new(),
            process_event_sender,
            process_event_receiver: Some(process_event_receiver),
            process_event_notify,
            kernel_reaper_task: None,
            kernel_reaper_deadline: None,
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
        self.javascript_engine.dispose_vm(vm_id);
        self.python_engine.dispose_vm(vm_id);
        self.wasm_engine.dispose_vm(vm_id);
        self.prune_extension_vm_resource(vm_id);
        self.extension_process_output_buffers
            .retain(|(buffer_vm_id, _process_id), _| buffer_vm_id != vm_id);
        if let Some(session) = self.sessions.get_mut(session_id) {
            session.vm_ids.remove(vm_id);
        }
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
            return Err(SidecarError::host(
                "ERR_AGENTOS_VM_GENERATION_LIMIT",
                format!("tracked={used} limit={limit}; raise runtime.resources.maxCapabilities"),
            ));
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
            return Err(SidecarError::host(
                "ERR_AGENTOS_VM_QUARANTINE_LIMIT",
                format!(
                    "quarantined={} limit={limit}; raise runtime.resources.maxCapabilities",
                    self.quarantined_vms.len()
                ),
            ));
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
            ActiveExecutionEvent::Common(agentos_execution::backend::ExecutionEvent::Output {
                stream,
                bytes,
            }) => {
                match stream {
                    agentos_execution::backend::OutputStream::Stdout => {
                        buffer.append_stdout(bytes.as_slice(), DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT)
                    }
                    agentos_execution::backend::OutputStream::Stderr => {
                        buffer.append_stderr(bytes.as_slice(), DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT)
                    }
                }
                true
            }
            ActiveExecutionEvent::Common(_) => false,
            ActiveExecutionEvent::Stdout(chunk) => {
                buffer.append_stdout(chunk, DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT);
                true
            }
            ActiveExecutionEvent::Stderr(chunk) => {
                buffer.append_stderr(chunk, DEFAULT_ACP_STDOUT_BUFFER_BYTE_LIMIT);
                true
            }
            ActiveExecutionEvent::HostRpcRequest(_)
            | ActiveExecutionEvent::HostCallCompletion(_)
            | ActiveExecutionEvent::ManagedStreamReadRecheck(_)
            | ActiveExecutionEvent::ManagedUdpPollRecheck(_)
            | ActiveExecutionEvent::SignalState { .. }
            | ActiveExecutionEvent::Exited(_) => false,
        }
    }

    fn bind_extension_process_resource(
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

    fn bind_extension_vm_resource(
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
            .saturating_sub(self.pending_process_events.len())
    }

    pub(crate) fn check_pending_process_event_capacity(
        &self,
        envelope: &ProcessEventEnvelope,
    ) -> Result<(), SidecarError> {
        let global_limit = self.config.runtime.protocol.max_process_events;
        if self.pending_process_events.len() >= global_limit {
            return Err(process_event_queue_overflow_error(global_limit));
        }
        let defaults = agentos_native_sidecar_core::limits::ProcessLimits::default();
        let limits = self
            .vms
            .get(&envelope.vm_id)
            .map(|vm| &vm.limits.process)
            .unwrap_or(&defaults);
        let mut vm_count = 0usize;
        let mut vm_bytes = 0usize;
        for pending in self
            .pending_process_events
            .iter()
            .filter(|pending| pending.vm_id == envelope.vm_id)
        {
            vm_count = vm_count.saturating_add(1);
            vm_bytes = vm_bytes.saturating_add(pending.retained_bytes());
        }
        if vm_count >= limits.pending_event_count {
            return Err(SidecarError::host_resource_limit(
                "limits.process.pendingEventCount",
                limits.pending_event_count,
                vm_count.saturating_add(1),
                format!(
                    "VM {} process event queue exceeded {} events (limits.process.pendingEventCount); raise limits.process.pendingEventCount",
                    envelope.vm_id, limits.pending_event_count
                ),
            ));
        }
        let next_bytes = vm_bytes.saturating_add(envelope.retained_bytes());
        if next_bytes > limits.pending_event_bytes {
            return Err(SidecarError::host_resource_limit(
                "limits.process.pendingEventBytes",
                limits.pending_event_bytes,
                next_bytes,
                format!(
                    "VM {} process event queue exceeded {} retained bytes (limits.process.pendingEventBytes); raise limits.process.pendingEventBytes",
                    envelope.vm_id, limits.pending_event_bytes
                ),
            ));
        }
        Ok(())
    }

    pub(crate) fn observe_pending_process_event_depth(&self) {
        self.pending_process_events_gauge
            .observe_depth(self.pending_process_events.len());
        self.pending_process_event_bytes_gauge.observe_depth(
            self.pending_process_events
                .iter()
                .fold(0usize, |bytes, event| {
                    bytes.saturating_add(event.retained_bytes())
                }),
        );
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
                    let error = SidecarError::host("ERR_AGENTOS_VM_TASK_FAILED", format!("vm_id={} class={:?} owner={} reason={:?}; dispose and recreate this VM generation",
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
            RequestRoute::Authenticate(payload) => {
                self.authenticate_connection(&request, payload).await
            }
            RequestRoute::OpenSession(payload) => self.open_session(&request, payload).await,
            RequestRoute::CreateVm(payload) => self.create_vm(&request, payload).await,
            RequestRoute::DisposeVm(payload) => self.dispose_vm(&request, payload).await,
            RequestRoute::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload.entries)
                    .await
            }
            RequestRoute::ConfigureVm(payload) => self.configure_vm(&request, payload).await,
            RequestRoute::RegisterHostCallbacks(payload) => {
                register_host_callbacks(self, &request, payload)
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
                self.execute_language_operation(&request, payload).await
            }
            RequestRoute::ExecutionLifecycle(payload) => {
                self.handle_execution_lifecycle(&request, payload).await
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
        let ctx = ExtensionContext::new(snapshot, self);
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

    pub(crate) async fn handle_process_event_envelope(
        &mut self,
        envelope: ProcessEventEnvelope,
    ) -> Result<Option<EventFrame>, SidecarError> {
        let handle_start = Instant::now();
        let ProcessEventEnvelope {
            connection_id,
            session_id,
            vm_id,
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
                    process_id: process_id.clone(),
                    event,
                })?;
                for event in trailing.into_iter().rev() {
                    self.queue_front_pending_process_event(ProcessEventEnvelope {
                        connection_id: connection_id.clone(),
                        session_id: session_id.clone(),
                        vm_id: vm_id.clone(),
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
            record_execute_phase(
                "process_exit_event_handle_envelope_total",
                handle_start.elapsed(),
            );
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

        self.connections.remove(connection_id);
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(events)
    }

    async fn authenticate_connection(
        &mut self,
        request: &RequestFrame,
        payload: crate::protocol::AuthenticateRequest,
    ) -> Result<DispatchResult, SidecarError> {
        self.connection_id_for(&request.ownership)?;
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

    async fn open_session(
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

    // kill_process_internal, handle_execution_event,
    // resolve_javascript_child_process_execution, spawn_child_process,
    // poll_child_process, write_child_process_stdin,
    // Child-process lifecycle operations moved to crate::execution.

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
        call: &ExecutionHostCall,
    ) -> Result<Option<crate::execution::HostServiceResponse>, SidecarError> {
        let request = &call.request;
        let requested_timeout_ms = match request.method.as_str() {
            "process.fd_write" => None,
            "__kernel_stdin_read" => parse_kernel_stdin_read_args(request)?.1,
            _ => {
                let timeout_ms = parse_kernel_poll_args(request)?.1;
                (timeout_ms >= 0).then_some(timeout_ms as u64)
            }
        };
        let now = Instant::now();

        let Some(vm) = self.vms.get_mut(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "deferred kernel wait RPC");
            return Ok(None);
        };
        let wait_handle = vm.kernel.poll_wait_handle();
        // Snapshot BEFORE the readiness probe: a write landing between the
        // probe and the waiter's wait bumps the generation, so the wait
        // returns immediately instead of losing the wakeup.
        let generation = wait_handle.snapshot();
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "deferred kernel wait RPC");
            return Ok(None);
        };
        let requested_timeout_ms = if request.method == "process.fd_write" {
            Some(vm.limits.reactor.operation_deadline_ms)
        } else {
            requested_timeout_ms
        };
        let requested_deadline = requested_timeout_ms
            .map(crate::execution::checked_deferred_guest_wait_deadline)
            .transpose()
            .map_err(SidecarError::from)?;
        // Reading from the pipe frees capacity. Top it off before every root
        // process read/poll probe, matching the descendant-process path, and
        // deliver a deferred close only after all accepted bytes are written.
        flush_pending_kernel_stdin(&mut vm.kernel, process)?;
        let kernel_pid = process.kernel_pid;
        let kernel_stdin_reader_fd = process.kernel_stdin_reader_fd;
        let same_parked_call = process
            .deferred_kernel_wait_rpc
            .as_ref()
            .is_some_and(|(parked, _)| parked.id == request.id);
        if !same_parked_call {
            process.deferred_kernel_wait_deadline_warned = false;
        }
        let deadline = if same_parked_call {
            process
                .deferred_kernel_wait_rpc
                .as_ref()
                .and_then(|(_, deadline)| *deadline)
        } else {
            requested_deadline
        };
        let probe = match request.method.as_str() {
            "process.fd_write" => {
                service_javascript_kernel_fd_write_sync_rpc(&mut vm.kernel, process, request)
            }
            "__kernel_stdin_read" => {
                let (max_bytes, _) = parse_kernel_stdin_read_args(request)?;
                kernel_stdin_read_response(
                    &mut vm.kernel,
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
                    && host_service_error_code(&error) == "EAGAIN" =>
            {
                (Value::Null, false)
            }
            Err(error) => {
                process.clear_deferred_kernel_wait_rpc();
                return Err(error);
            }
        };
        let mut operation_deadline = if request.method == "process.fd_write" {
            deadline.map(|deadline| {
                crate::execution::OperationDeadlineTracker::from_deadline(
                    deadline,
                    Duration::from_millis(vm.limits.reactor.operation_deadline_ms),
                    process.deferred_kernel_wait_deadline_warned,
                )
            })
        } else {
            None
        };
        if !ready {
            if let Some(deadline) = operation_deadline.as_mut() {
                deadline.observe("deferred root-process fd write");
                process.deferred_kernel_wait_deadline_warned = deadline.warning_emitted();
            }
        }
        if request.method == "process.fd_write"
            && !ready
            && deadline.is_some_and(|deadline| now >= deadline)
        {
            process.clear_deferred_kernel_wait_rpc();
            return Err(SidecarError::host("ETIMEDOUT", format!("pipe write exceeded limits.reactor.operationDeadlineMs ({} ms); raise that limit for slower readers",
                vm.limits.reactor.operation_deadline_ms
            )));
        }
        if ready
            || requested_timeout_ms == Some(0)
            || deadline.is_some_and(|deadline| now >= deadline)
        {
            process.clear_deferred_kernel_wait_rpc();
            return Ok(Some(probe.into()));
        }

        let connection_id = vm.connection_id.clone();
        let session_id = vm.session_id.clone();
        let runtime = vm.runtime_context.clone();
        let remaining = operation_deadline
            .as_ref()
            .map(crate::execution::OperationDeadlineTracker::remaining_until_next_edge)
            .or_else(|| deadline.map(|deadline| deadline.saturating_duration_since(now)));
        let sender = self.process_event_sender.clone();
        let event_notify = Arc::clone(&self.process_event_notify);
        let waiter_request = call.clone();
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
                    process_id: envelope_process_id,
                    event: ActiveExecutionEvent::HostRpcRequest(waiter_request),
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
        let Some(vm) = self.vms.get_mut(vm_id) else {
            return Ok(None);
        };
        let Some(process) = vm.active_processes.get_mut(process_id) else {
            return Ok(None);
        };
        process.deferred_kernel_wait_rpc = Some((call.clone(), deadline));
        Ok(None)
    }

    pub(crate) async fn handle_javascript_sync_rpc_request(
        &mut self,
        vm_id: &str,
        process_id: &str,
        call: ExecutionHostCall,
    ) -> Result<(), SidecarError> {
        let request = &call.request;
        record_sync_bridge_request_observed(request.id, &request.method);
        let Some(vm) = self.vms.get(vm_id) else {
            log_stale_process_event(&self.bridge, vm_id, process_id, "javascript sync RPC");
            return Ok(());
        };
        if !vm.active_processes.contains_key(process_id) {
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

        let response: Result<crate::execution::HostServiceResponse, SidecarError> = match request
            .method
            .as_str()
        {
            _ if deferrable_fd_write.is_some() => {
                let normalized = deferrable_fd_write
                    .as_ref()
                    .expect("guarded deferred fd_write request");
                let normalized_call = ExecutionHostCall {
                    request: normalized.clone(),
                    reply: call.reply.clone(),
                };
                match self.service_deferrable_kernel_wait_rpc(vm_id, process_id, &normalized_call) {
                    Ok(Some(response)) => Ok(response),
                    Ok(None) => return Ok(()),
                    Err(error) => Err(error),
                }
            }
            "child_process.spawn" => {
                let Some(vm) = self.vms.get(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "javascript sync RPC child_process.spawn",
                    );
                    return Ok(());
                };
                let (payload, _) = parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.spawn_child_process(vm_id, process_id, payload)
                    .await
                    .map(Into::into)
            }
            "child_process.spawn_sync" => {
                let Some(vm) = self.vms.get(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "javascript sync RPC child_process.spawn_sync",
                    );
                    return Ok(());
                };
                let (payload, max_buffer) =
                    parse_javascript_child_process_spawn_request(vm, &request.args)?;
                self.defer_javascript_child_process_sync(vm_id, process_id, payload, max_buffer)
                    .await
            }
            "child_process.poll" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.poll child id")?;
                let wait_ms = javascript_sync_rpc_arg_u64_optional(
                    &request.args,
                    1,
                    "child_process.poll wait ms",
                )?
                .unwrap_or_default();
                self.poll_child_process(vm_id, process_id, child_process_id, wait_ms)
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
                self.write_child_process_stdin(vm_id, process_id, child_process_id, &chunk)?;
                Ok(Value::Null.into())
            }
            "child_process.close_stdin" => {
                let child_process_id = javascript_sync_rpc_arg_str(
                    &request.args,
                    0,
                    "child_process.close_stdin child id",
                )?;
                self.close_child_process_stdin(vm_id, process_id, child_process_id)?;
                Ok(Value::Null.into())
            }
            "child_process.kill" => {
                let child_process_id =
                    javascript_sync_rpc_arg_str(&request.args, 0, "child_process.kill child id")?;
                let signal =
                    javascript_sync_rpc_arg_str(&request.args, 1, "child_process.kill signal")?;
                self.kill_javascript_child_process(vm_id, process_id, child_process_id, signal)?;
                Ok(Value::Null.into())
            }
            "process.kill" => {
                let target_pid =
                    javascript_sync_rpc_arg_i32(&request.args, 0, "process.kill target pid")?;
                let signal = javascript_sync_rpc_arg_str(&request.args, 1, "process.kill signal")?;
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
                        } else if let Some((child_process_id, _)) = caller
                            .child_processes
                            .iter()
                            .find(|(_, child)| i32::try_from(child.kernel_pid) == Ok(target_pid))
                        {
                            ProcessKillTarget::Child(child_process_id.clone())
                        } else if let Some((target_process_id, _)) =
                            vm.active_processes.iter().find(|(_, process)| {
                                i32::try_from(process.kernel_pid) == Ok(target_pid)
                            })
                        {
                            ProcessKillTarget::TopLevel(target_process_id.clone())
                        } else {
                            let target_kernel_pid = u32::try_from(target_pid).map_err(|_| {
                                SidecarError::host(
                                    "EINVAL",
                                    format!("invalid process pid {target_pid}"),
                                )
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
                    .map_err(SidecarError::from)?;
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "javascript sync RPC process.signal_state",
                    );
                    return Ok(());
                };
                let process = vm.active_processes.get(process_id).ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "VM {vm_id} has no active process {process_id}"
                    ))
                })?;
                apply_kernel_signal_registration(process, signal, &registration)?;
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
                    return Err(SidecarError::host(
                        "EACCES",
                        format!(
                            "HTTP loopback request requires a loopback host, got {}",
                            payload.host
                        ),
                    ));
                }
                self.bridge.require_network_access(
                    vm_id,
                    NetworkOperation::Http,
                    format_tcp_resource(&payload.host, payload.port),
                )?;
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "javascript sync RPC net.http_request",
                    );
                    return Ok(());
                };
                let socket_paths = build_socket_path_context(vm)?;
                let target_is_current =
                    [SocketFamily::Ipv4, SocketFamily::Ipv6]
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
                let Some(target_process) = vm.active_processes.get_mut(&payload.process_id) else {
                    return Err(SidecarError::InvalidState(format!(
                        "unknown HTTP loopback process {}",
                        payload.process_id
                    )));
                };
                dispatch_loopback_http_request_deferred(LoopbackHttpDispatchRequest {
                    process: target_process,
                    server_id: payload.server_id,
                    request_json: &payload.request,
                })
            }
            "__kernel_stdio_write"
                if self
                    .vms
                    .get(vm_id)
                    .and_then(|vm| vm.active_processes.get(process_id))
                    .is_some_and(|process| process.tty_master_owner.is_some()) =>
            {
                let (writer_kernel_pid, owner) = {
                    let process = self
                        .vms
                        .get(vm_id)
                        .and_then(|vm| vm.active_processes.get(process_id))
                        .expect("guarded by match arm");
                    (
                        process.kernel_pid,
                        process.tty_master_owner.expect("guarded by match arm"),
                    )
                };
                self.service_shared_tty_stdio_write(vm_id, writer_kernel_pid, owner, &request)
                    .map(Into::into)
            }
            "__kernel_stdin_read" | "__kernel_poll" => {
                match self.service_deferrable_kernel_wait_rpc(vm_id, process_id, &call) {
                    Ok(Some(response)) => Ok(response),
                    // Parked: an off-loop waiter re-enqueues this request as a
                    // process event when kernel poll state changes.
                    Ok(None) => return Ok(()),
                    Err(error) => Err(error),
                }
            }
            _ => {
                let Some(vm) = self.vms.get_mut(vm_id) else {
                    log_stale_process_event(
                        &self.bridge,
                        vm_id,
                        process_id,
                        "javascript sync RPC bridge dispatch",
                    );
                    return Ok(());
                };
                let socket_paths = build_socket_path_context(vm)?;
                let kernel_readiness = Arc::clone(&vm.kernel_socket_readiness);
                let capabilities = vm.capabilities.clone();
                let managed_descriptions = Arc::clone(&vm.managed_host_net_descriptions);
                let Some(process) = vm.active_processes.get_mut(process_id) else {
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
                    dns: &vm.dns,
                    socket_paths: &socket_paths,
                    kernel: &mut vm.kernel,
                    kernel_readiness,
                    process,
                    sync_request: &request,
                    capabilities,
                    managed_descriptions: Some(managed_descriptions),
                })
                .await
            }
        };

        let response = match response {
            Ok(crate::execution::HostServiceResponse::Deferred {
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
                let reply = call.reply.clone();
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
                                details: None,
                            })
                        })
                    };
                    let result = match timeout {
                        Some(timeout) => match crate::execution::operation_deadline_timeout(
                            &method,
                            timeout,
                            receive,
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => Err(crate::state::DeferredRpcError {
                                code: String::from("ERR_AGENTOS_DEFERRED_RPC_TIMEOUT"),
                                message: format!(
                                    "{method} exceeded limits.reactor.operationDeadlineMs ({} ms); raise that limit for slower peers",
                                    timeout.as_millis()
                                ),
                                details: None,
                            }),
                        },
                        None => receive.await,
                    };
                    if sender
                        .send(ProcessEventEnvelope {
                            connection_id,
                            session_id,
                            vm_id: envelope_vm_id,
                            process_id: envelope_process_id,
                            event: ActiveExecutionEvent::HostCallCompletion(
                                crate::state::HostCallCompletion { reply, result },
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
            if let Some(vm) = self.vms.get_mut(vm_id) {
                Self::wake_ready_deferred_fd_reads(vm)?;
            }
        }
        if response.is_ok() && javascript_sync_rpc_may_make_fd_writable(&request) {
            if let Some(vm) = self.vms.get_mut(vm_id) {
                Self::wake_ready_deferred_fd_writes(vm)?;
            }
        }

        let Some(vm) = self.vms.get_mut(vm_id) else {
            log_stale_process_event(
                &self.bridge,
                vm_id,
                process_id,
                "javascript sync RPC response delivery",
            );
            return Ok(());
        };
        if !vm.active_processes.contains_key(process_id) {
            log_stale_process_event(
                &self.bridge,
                vm_id,
                process_id,
                "javascript sync RPC response delivery",
            );
            return Ok(());
        }

        if let Err(error) = &response {
            tracing::warn!(
                method = %call.request.method,
                error = %error,
                "executor host RPC failed"
            );
        }
        settle_execution_host_call(&call.reply, response)
    }

    /// Applies a `process.kill` aimed at the calling process itself.
    ///
    /// Signal delivery is exclusively kernel-owned. In particular, do not
    /// return the legacy `self` action payload: the JavaScript shim would
    /// synchronously deliver that payload in addition to the runtime-neutral
    /// checkpoint already published by `kill_process_internal`.
    fn apply_self_process_kill(
        &mut self,
        vm_id: &str,
        process_id: &str,
        parsed_signal: i32,
    ) -> Result<Value, SidecarError> {
        self.kill_process_internal(vm_id, process_id, &parsed_signal.to_string())?;
        Ok(Value::Null)
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
            return Err(SidecarError::host("ERR_AGENTOS_VM_QUARANTINED", format!("vm_id={vm_id} generation={} reason={:?} active_tasks={} outstanding_capabilities={} ledger_zero={} integrity_ok={}",
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
        if let SidecarError::Host(host_error) = error {
            if host_error.code == "ERR_AGENTOS_RESOURCE_LIMIT" {
                let details = host_error.details.as_ref();
                let limit_name = details
                    .and_then(|value| value.get("limitName"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let configured_limit = details
                    .and_then(|value| value.get("limit"))
                    .and_then(Value::as_u64);
                let requested = details
                    .and_then(|value| value.get("observed"))
                    .and_then(Value::as_u64);
                let configuration_path = details
                    .and_then(|value| value.get("configPath"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| limit_name.clone());
                let vm_id = match &request.ownership {
                    OwnershipScope::VmOwnership(owner) => Some(owner.vm_id.clone()),
                    OwnershipScope::ConnectionOwnership(_)
                    | OwnershipScope::SessionOwnership(_) => None,
                };
                let session_generation = vm_id
                    .as_ref()
                    .and_then(|vm_id| self.vms.get(vm_id))
                    .map(|vm| vm.generation);
                return self.respond(
                    request,
                    ResponsePayload::Rejected(RejectedResponse {
                        code: host_error.code.clone(),
                        message: host_error.message.clone(),
                        limit_name,
                        configured_limit,
                        current_usage: None,
                        requested,
                        unit: Some(String::from("items")),
                        scope: Some(if vm_id.is_some() {
                            String::from("vm")
                        } else {
                            String::from("process")
                        }),
                        vm_id,
                        session_generation,
                        capability_id: None,
                        operation: None,
                        configuration_path,
                        retryable: Some(false),
                        errno: Some(String::from("ENOBUFS")),
                    }),
                );
            }
        }
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
        let completed_limit = self.config.runtime.protocol.max_completed_responses;
        if self.completed_sidecar_responses.len() >= completed_limit {
            return Err(SidecarError::host_resource_limit(
                "runtime.protocol.maxCompletedResponses",
                completed_limit,
                self.completed_sidecar_responses.len().saturating_add(1),
                format!(
                    "completed sidecar response queue reached {completed_limit} retained responses; drain responses or raise runtime.protocol.maxCompletedResponses"
                ),
            ));
        }
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
        fn request_shutdown(process: &mut crate::state::ActiveProcess) {
            for child in process.child_processes.values_mut() {
                request_shutdown(child);
            }
            if let Err(error) = process.execution.terminate() {
                eprintln!(
                    "ERR_AGENTOS_PROCESS_DROP_SHUTDOWN: failed to request shutdown for kernel pid {}: {error}",
                    process.kernel_pid
                );
            }
        }

        // Execution engines are declared before `vms`, so Rust's default field
        // drop order would tear the engines down while VM-owned execution
        // handles are still live. Request backend shutdown and release every VM
        // generation first so sessions can drain against a live engine/runtime.
        for vm in self.vms.values_mut() {
            for process in vm.active_processes.values_mut() {
                request_shutdown(process);
            }
        }
        self.vms.clear();
        self.quarantined_vms.clear();
        if let Some(task) = self.kernel_reaper_task.take() {
            task.abort();
        }
    }
}

impl<B> ExtensionHost for NativeSidecar<B>
where
    B: NativeSidecarBridge + Send + 'static,
    BridgeError<B>: fmt::Debug + Send + Sync + 'static,
{
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
            let (connection_id, session_id, vm_id) = self.vm_scope_for(&ownership)?;
            self.require_owned_vm(&connection_id, &session_id, &vm_id)?;
            let key = (vm_id.clone(), process_id.clone());
            let deadline = Instant::now() + timeout;
            let process_event_notify = Arc::clone(&self.process_event_notify);
            loop {
                // Register before probing the durable process-event queues so
                // an arrival racing this turn cannot be lost before the await.
                let notified = process_event_notify.notified();
                self.pump_process_events(&ownership).await?;
                while let Some(envelope) =
                    self.take_matching_process_event_envelope(&vm_id, &process_id)?
                {
                    if self.capture_extension_process_output_event(
                        &vm_id,
                        &process_id,
                        &envelope.event,
                    ) {
                        continue;
                    }
                    self.queue_pending_process_event(envelope)?;
                    break;
                }
                let buffered = self
                    .extension_process_output_buffers
                    .get(&key)
                    .is_some_and(|buffer| !buffer.stdout.is_empty() || !buffer.stderr.is_empty());
                if buffered || timeout.is_zero() || Instant::now() >= deadline {
                    break;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                tokio::select! {
                    _ = notified => {}
                    _ = time::sleep(remaining) => break,
                }
            }
            self.bind_extension_process_resource(
                ownership,
                namespace,
                ext_session_id,
                process_id.clone(),
            )?;
            self.extension_process_output_buffers
                .remove(&key)
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "extension process output buffering was not started",
                    ))
                })
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
    if let Err(error) = bridge.emit_log(
        vm_id,
        format!(
            "Ignoring stale process event during {context}: VM {vm_id} process {process_id} was already reaped"
        ),
    ) {
        eprintln!(
            "ERR_AGENTOS_DIAGNOSTIC_EMIT: failed to emit stale process diagnostic for VM {vm_id} process {process_id}: {error:?}"
        );
    }
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
    SidecarError::Host(agentos_execution::backend::HostServiceError::new(
        error.code(),
        error.to_string(),
    ))
}

pub(crate) fn plugin_error(error: PluginError) -> SidecarError {
    SidecarError::Plugin(error.to_string())
}

pub(crate) fn javascript_error(error: JavascriptExecutionError) -> SidecarError {
    match error {
        JavascriptExecutionError::EventChannelClosed => SidecarError::ExecutionEventChannelClosed {
            backend: ExecutionBackendKind::Javascript,
        },
        other => SidecarError::Execution(other.to_string()),
    }
}

pub(crate) fn wasm_error(error: WasmExecutionError) -> SidecarError {
    let message = error.to_string();
    match error {
        WasmExecutionError::EventChannelClosed => SidecarError::ExecutionEventChannelClosed {
            backend: ExecutionBackendKind::WebAssembly,
        },
        WasmExecutionError::NativeBinaryNotSupported { .. } => {
            SidecarError::host("ERR_NATIVE_BINARY_NOT_SUPPORTED", message)
        }
        WasmExecutionError::DeterministicFuelUnsupported { .. } => {
            SidecarError::host("ENOTSUP", message)
        }
        _ => SidecarError::Execution(message),
    }
}

#[cfg(test)]
mod execution_error_tests {
    use super::*;
    use agentos_execution::wasm::NativeBinaryFormat;

    #[test]
    fn native_binary_rejection_preserves_its_guest_error_code() {
        let error = wasm_error(WasmExecutionError::NativeBinaryNotSupported {
            path: PathBuf::from("/tmp/fake-rg"),
            header: vec![0x7f, b'E', b'L', b'F'],
            format: NativeBinaryFormat::Elf,
        });

        assert_eq!(error.code(), Some("ERR_NATIVE_BINARY_NOT_SUPPORTED"));
    }

    #[test]
    fn deterministic_fuel_rejection_preserves_enotsup() {
        let error = wasm_error(WasmExecutionError::DeterministicFuelUnsupported { fuel: 42 });

        assert_eq!(error.code(), Some("ENOTSUP"));
        assert!(error.to_string().contains("deterministic WebAssembly fuel"));
    }

    #[test]
    fn closed_execution_channels_preserve_the_backend_kind() {
        assert_eq!(
            javascript_error(JavascriptExecutionError::EventChannelClosed),
            SidecarError::ExecutionEventChannelClosed {
                backend: ExecutionBackendKind::Javascript,
            }
        );
        assert_eq!(
            python_error(PythonExecutionError::EventChannelClosed),
            SidecarError::ExecutionEventChannelClosed {
                backend: ExecutionBackendKind::Python,
            }
        );
        assert_eq!(
            wasm_error(WasmExecutionError::EventChannelClosed),
            SidecarError::ExecutionEventChannelClosed {
                backend: ExecutionBackendKind::WebAssembly,
            }
        );
    }
}

pub(crate) fn python_error(error: PythonExecutionError) -> SidecarError {
    match error {
        PythonExecutionError::EventChannelClosed => SidecarError::ExecutionEventChannelClosed {
            backend: ExecutionBackendKind::Python,
        },
        other => SidecarError::Execution(other.to_string()),
    }
}

pub(crate) fn vfs_error(error: VfsError) -> SidecarError {
    SidecarError::host(error.code(), error.message())
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
        assert!(!options
            .internal_bootstrap_env
            .contains_key("AGENTOS_WASM_INITIAL_SIGNAL_MASK"));
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
        assert!(!hint.contains("agentos"));
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
            _ctx: ExtensionContext<'a>,
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
