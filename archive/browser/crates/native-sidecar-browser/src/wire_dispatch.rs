use crate::{
    BrowserExecutionOptions, BrowserExtensionRequest, BrowserSidecar, BrowserSidecarBridge,
    BrowserSidecarConfig,
};
use agentos_bridge::{
    CreateJavascriptContextRequest, CreateWasmContextRequest, ExecutionEvent,
    ExecutionHandleRequest, GuestRuntime, KillExecutionRequest, PollExecutionEventRequest,
    StartExecutionRequest, WriteExecutionStdinRequest,
};
use agentos_kernel::kernel::KernelVmConfig;
use agentos_native_sidecar_core::{
    authenticated_response, bound_udp_snapshot_response, connection_id_of,
    execution_signal_from_number, layer_created_response, layer_sealed_response,
    listener_snapshot_response, overlay_created_response, permissions_from_policy,
    process_exited_event, process_killed_response, process_output_event, process_snapshot_response,
    process_started_response, protocol_process_snapshot_entry, protocol_root_filesystem_mode,
    reject, respond, root_filesystem_bootstrapped_response, root_filesystem_snapshot_response,
    root_snapshot_entry, route_request_payload, session_opened_response, session_scope_of,
    signal_state_response, snapshot_exported_response, snapshot_imported_response,
    stdin_closed_response, stdin_written_response, unsupported_guest_kernel_call_event,
    unsupported_host_callback_direction_dispatch, validate_authenticate_versions,
    vm_configured_response, vm_created_response, vm_disposed_response, vm_id_of,
    vm_lifecycle_event, zombie_timer_count_response, DispatchResult, RequestRoute,
};
use agentos_sidecar_protocol::protocol::{
    AuthenticateRequest, BootstrapRootFilesystemRequest, CloseStdinRequest, ConfigureVmRequest,
    CreateLayerRequest, CreateOverlayRequest, CreateVmRequest, DisposeVmRequest, EventFrame,
    ExecuteRequest, ExportSnapshotRequest, ExtEnvelope, FindBoundUdpRequest, FindListenerRequest,
    GetProcessSnapshotRequest, GetSignalStateRequest, GetZombieTimerCountRequest, GuestRuntimeKind,
    HostCallbacksRegisteredResponse, ImportSnapshotRequest, KillProcessRequest, OpenSessionRequest,
    OwnershipScope, RegisterHostCallbacksRequest, RequestFrame, ResponsePayload, SealLayerRequest,
    SnapshotRootFilesystemRequest, SocketStateEntry, StreamChannel, VmFetchRequest,
    VmFetchResponse, VmLifecycleState, WriteStdinRequest,
};
use agentos_sidecar_protocol::wire::{
    request_frame_to_compat, CompatDispatchResult, ProtocolCodecError, ProtocolFrame,
    WireFrameCodec,
};
use agentos_vm_config::CreateVmConfig;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

pub const BROWSER_SIDECAR_ID: &str = "agentos-native-sidecar-browser";
pub const BROWSER_MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExecutionRecord {
    vm_id: String,
    process_id: String,
    ownership: OwnershipScope,
}

type ProcessExecutionKey = (String, String);

pub struct BrowserWireDispatcher<B: BrowserSidecarBridge> {
    codec: WireFrameCodec,
    sidecar: BrowserSidecar<B>,
    next_connection: usize,
    next_session: usize,
    next_vm: usize,
    active_vms: BTreeSet<String>,
    executions: BTreeMap<String, ExecutionRecord>,
    process_executions: BTreeMap<ProcessExecutionKey, String>,
    pending_events: VecDeque<EventFrame>,
}

impl<B> BrowserWireDispatcher<B>
where
    B: BrowserSidecarBridge,
    <B as agentos_bridge::BridgeTypes>::Error: fmt::Debug,
{
    pub fn new(bridge: B) -> Self {
        Self {
            codec: WireFrameCodec::new(BROWSER_MAX_FRAME_BYTES),
            sidecar: BrowserSidecar::new(bridge, BrowserSidecarConfig::default()),
            next_connection: 0,
            next_session: 0,
            next_vm: 0,
            active_vms: BTreeSet::new(),
            executions: BTreeMap::new(),
            process_executions: BTreeMap::new(),
            pending_events: VecDeque::new(),
        }
    }

    pub fn vm_count(&self) -> usize {
        self.sidecar.vm_count()
    }

    pub fn sidecar_mut(&mut self) -> &mut BrowserSidecar<B> {
        &mut self.sidecar
    }

    pub fn handle_request_bytes(&mut self, bytes: &[u8]) -> Result<Vec<u8>, ProtocolCodecError> {
        let generated_request = match self.codec.decode_message(bytes)? {
            ProtocolFrame::RequestFrame(request) => request,
            _ => {
                return Err(ProtocolCodecError::SerializeFailure(String::from(
                    "browser sidecar expected a request frame",
                )));
            }
        };
        let request = request_frame_to_compat(generated_request)?;
        let dispatch = self.dispatch(request);
        for event in dispatch.events.iter().cloned() {
            self.pending_events.push_back(event);
        }
        let generated =
            agentos_sidecar_protocol::wire::dispatch_result_from_compat(CompatDispatchResult {
                response: dispatch.response,
                events: Vec::new(),
            })?;
        self.codec
            .encode_message(&ProtocolFrame::ResponseFrame(generated.response))
    }

    pub fn poll_event_bytes(&mut self) -> Result<Option<Vec<u8>>, ProtocolCodecError> {
        if self.pending_events.is_empty() {
            self.pump_execution_events();
        }
        let Some(event) = self.pending_events.pop_front() else {
            return Ok(None);
        };
        let generated = agentos_sidecar_protocol::wire::event_frame_from_compat(event)?;
        self.codec
            .encode_message(&ProtocolFrame::EventFrame(generated))
            .map(Some)
    }

    fn dispatch(&mut self, request: RequestFrame) -> DispatchResult {
        match route_request_payload(&request) {
            RequestRoute::Authenticate(payload) => self.authenticate(&request, payload),
            RequestRoute::OpenSession(payload) => self.open_session(&request, payload),
            RequestRoute::CreateVm(payload) => self.create_vm(&request, payload),
            RequestRoute::DisposeVm(payload) => self.dispose_vm(&request, payload),
            RequestRoute::BootstrapRootFilesystem(payload) => {
                self.bootstrap_root_filesystem(&request, payload)
            }
            RequestRoute::ConfigureVm(payload) => self.configure_vm(&request, payload),
            RequestRoute::RegisterHostCallbacks(payload) => {
                self.register_host_callbacks(&request, payload)
            }
            RequestRoute::CreateLayer(payload) => self.create_layer(&request, payload),
            RequestRoute::SealLayer(payload) => self.seal_layer(&request, payload),
            RequestRoute::ImportSnapshot(payload) => self.import_snapshot(&request, payload),
            RequestRoute::ExportSnapshot(payload) => self.export_snapshot(&request, payload),
            RequestRoute::CreateOverlay(payload) => self.create_overlay(&request, payload),
            RequestRoute::GuestFilesystemCall(payload) => {
                self.guest_filesystem_call(&request, payload)
            }
            RequestRoute::GuestKernelCall(payload) => self.guest_kernel_call(&request, payload),
            RequestRoute::SnapshotRootFilesystem(payload) => {
                self.snapshot_root_filesystem(&request, payload)
            }
            RequestRoute::GetProcessSnapshot(payload) => {
                self.get_process_snapshot(&request, payload)
            }
            RequestRoute::GetResourceSnapshot(payload) => {
                // Resource snapshots surface the native sidecar's queue/limit
                // trackers, which the converged browser runtime does not run.
                let _ = payload;
                rejected(
                    &request,
                    "unsupported",
                    "get_resource_snapshot is not available in the converged browser runtime",
                )
            }
            RequestRoute::GetSignalState(payload) => self.get_signal_state(&request, payload),
            RequestRoute::GetZombieTimerCount(payload) => {
                self.get_zombie_timer_count(&request, payload)
            }
            RequestRoute::Execute(payload) => self.execute(&request, payload),
            RequestRoute::WriteStdin(payload) => self.write_stdin(&request, payload),
            RequestRoute::ResizePty(payload) => {
                // The converged browser path resizes the PTY through the driver's
                // master-side resize, not via a native wire ResizePty op, so this
                // route is not exercised by the in-browser terminal.
                let _ = payload;
                rejected(
                    &request,
                    "unsupported",
                    "resize_pty is handled by the converged browser driver, not the native wire op",
                )
            }
            RequestRoute::CloseStdin(payload) => self.close_stdin(&request, payload),
            RequestRoute::KillProcess(payload) => self.kill_process(&request, payload),
            RequestRoute::FindListener(payload) => self.find_listener(&request, payload),
            RequestRoute::FindBoundUdp(payload) => self.find_bound_udp(&request, payload),
            RequestRoute::VmFetch(payload) => self.vm_fetch(&request, payload),
            RequestRoute::Ext(payload) => self.ext(&request, payload),
            RequestRoute::LinkPackage(payload) => {
                // Package linking projects host-filesystem package trees into the
                // VM, which the converged browser runtime does not provide.
                let _ = payload;
                rejected(
                    &request,
                    "unsupported",
                    "link_package is not available in the converged browser runtime",
                )
            }
            RequestRoute::ProvidedCommands(payload) => {
                // Provided command metadata is retained by the native sidecar's
                // host-backed package projection, which the converged browser
                // runtime does not provide.
                let _ = payload;
                rejected(
                    &request,
                    "unsupported",
                    "provided_commands is not available in the converged browser runtime",
                )
            }
            RequestRoute::UnsupportedHostCallbackDirection => {
                unsupported_host_callback_direction_dispatch(&request)
            }
        }
    }

    fn bootstrap_root_filesystem(
        &mut self,
        request: &RequestFrame,
        payload: BootstrapRootFilesystemRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "bootstrap_root_filesystem requires VM ownership",
            );
        };
        let entry_count = match self
            .sidecar
            .bootstrap_root_filesystem_entries(&vm_id, &payload.entries)
        {
            Ok(entry_count) => entry_count,
            Err(error) => return rejected(request, "bootstrap_root_failed", &error.to_string()),
        };
        DispatchResult {
            response: root_filesystem_bootstrapped_response(request, entry_count),
            events: Vec::new(),
        }
    }

    fn configure_vm(
        &mut self,
        request: &RequestFrame,
        payload: ConfigureVmRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "configure_vm requires VM ownership",
            );
        };
        if !payload.mounts.is_empty()
            || !payload.software.is_empty()
            || payload.module_access_cwd.is_some()
        {
            return rejected(
                request,
                "unsupported_request",
                "browser ConfigureVm does not support host mounts, software installs, or moduleAccessCwd",
            );
        }

        let permissions = match payload.permissions {
            Some(policy) => {
                let policy =
                    agentos_sidecar_protocol::wire::permissions_policy_config_from_wire(policy);
                if let Err(error) =
                    agentos_native_sidecar_core::validate_permissions_policy(&policy)
                {
                    return rejected(request, "invalid_config", &error.to_string());
                }
                Some(permissions_from_policy(policy))
            }
            None => None,
        };
        if let Err(error) = self.sidecar.configure_vm(
            &vm_id,
            permissions,
            payload.instructions,
            payload.projected_modules,
            payload.command_permissions.into_iter().collect(),
            payload.loopback_exempt_ports,
        ) {
            return rejected(request, "configure_vm_failed", &error.to_string());
        }
        DispatchResult {
            response: vm_configured_response(request, 0, 0, Vec::new(), Vec::new()),
            events: Vec::new(),
        }
    }

    fn register_host_callbacks(
        &mut self,
        request: &RequestFrame,
        payload: RegisterHostCallbacksRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "register_host_callbacks requires VM ownership",
            );
        };
        let (registration, command_count) =
            match self.sidecar.register_host_callbacks(&vm_id, payload) {
                Ok(result) => result,
                Err(error) => {
                    return rejected(
                        request,
                        "register_host_callbacks_failed",
                        &error.to_string(),
                    )
                }
            };
        DispatchResult {
            response: respond(
                request,
                ResponsePayload::HostCallbacksRegistered(HostCallbacksRegisteredResponse {
                    registration,
                    command_count,
                }),
            ),
            events: Vec::new(),
        }
    }

    fn guest_filesystem_call(
        &mut self,
        request: &RequestFrame,
        payload: agentos_sidecar_protocol::protocol::GuestFilesystemCallRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "guest_filesystem_call requires VM ownership",
            );
        };
        let result = match self.sidecar.guest_filesystem_call(&vm_id, payload) {
            Ok(result) => result,
            Err(error) => return rejected(request, "guest_filesystem_failed", &error.to_string()),
        };
        DispatchResult {
            response: respond(request, ResponsePayload::GuestFilesystemResult(result)),
            events: Vec::new(),
        }
    }

    fn guest_kernel_call(
        &mut self,
        request: &RequestFrame,
        payload: agentos_sidecar_protocol::protocol::GuestKernelCallRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "guest_kernel_call requires VM ownership",
            );
        };
        let result = match self.sidecar.guest_kernel_call(&vm_id, payload) {
            Ok(result) => result,
            Err(error) => return rejected(request, "guest_kernel_failed", &error.to_string()),
        };
        DispatchResult {
            response: respond(request, ResponsePayload::GuestKernelResult(result)),
            events: Vec::new(),
        }
    }

    fn create_layer(
        &mut self,
        request: &RequestFrame,
        _payload: CreateLayerRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "create_layer requires VM ownership",
            );
        };
        let layer_id = match self.sidecar.create_layer(&vm_id) {
            Ok(layer_id) => layer_id,
            Err(error) => return rejected(request, "create_layer_failed", &error.to_string()),
        };
        DispatchResult {
            response: layer_created_response(request, layer_id),
            events: Vec::new(),
        }
    }

    fn seal_layer(&mut self, request: &RequestFrame, payload: SealLayerRequest) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "seal_layer requires VM ownership",
            );
        };
        let layer_id = match self.sidecar.seal_layer(&vm_id, &payload.layer_id) {
            Ok(layer_id) => layer_id,
            Err(error) => return rejected(request, "seal_layer_failed", &error.to_string()),
        };
        DispatchResult {
            response: layer_sealed_response(request, layer_id),
            events: Vec::new(),
        }
    }

    fn import_snapshot(
        &mut self,
        request: &RequestFrame,
        payload: ImportSnapshotRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "import_snapshot requires VM ownership",
            );
        };
        let layer_id = match self.sidecar.import_snapshot(&vm_id, &payload.entries) {
            Ok(layer_id) => layer_id,
            Err(error) => return rejected(request, "import_snapshot_failed", &error.to_string()),
        };
        DispatchResult {
            response: snapshot_imported_response(request, layer_id),
            events: Vec::new(),
        }
    }

    fn export_snapshot(
        &mut self,
        request: &RequestFrame,
        payload: ExportSnapshotRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "export_snapshot requires VM ownership",
            );
        };
        let snapshot = match self.sidecar.export_snapshot(&vm_id, &payload.layer_id) {
            Ok(snapshot) => snapshot,
            Err(error) => return rejected(request, "export_snapshot_failed", &error.to_string()),
        };
        DispatchResult {
            response: snapshot_exported_response(
                request,
                payload.layer_id,
                snapshot.entries.iter().map(root_snapshot_entry).collect(),
            ),
            events: Vec::new(),
        }
    }

    fn create_overlay(
        &mut self,
        request: &RequestFrame,
        payload: CreateOverlayRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "create_overlay requires VM ownership",
            );
        };
        let mode = protocol_root_filesystem_mode(payload.mode);
        let layer_id = match self.sidecar.create_overlay(
            &vm_id,
            mode,
            payload.upper_layer_id,
            payload.lower_layer_ids,
        ) {
            Ok(layer_id) => layer_id,
            Err(error) => return rejected(request, "create_overlay_failed", &error.to_string()),
        };
        DispatchResult {
            response: overlay_created_response(request, layer_id),
            events: Vec::new(),
        }
    }

    fn snapshot_root_filesystem(
        &mut self,
        request: &RequestFrame,
        _payload: SnapshotRootFilesystemRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "snapshot_root_filesystem requires VM ownership",
            );
        };
        let snapshot = match self.sidecar.snapshot_root_filesystem(&vm_id) {
            Ok(snapshot) => snapshot,
            Err(error) => return rejected(request, "snapshot_root_failed", &error.to_string()),
        };
        DispatchResult {
            response: root_filesystem_snapshot_response(
                request,
                snapshot.entries.iter().map(root_snapshot_entry).collect(),
            ),
            events: Vec::new(),
        }
    }

    fn get_process_snapshot(
        &mut self,
        request: &RequestFrame,
        _payload: GetProcessSnapshotRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "get_process_snapshot requires VM ownership",
            );
        };
        let mut processes = match self.sidecar.process_snapshot_entries(&vm_id) {
            Ok(processes) => processes,
            Err(error) => return rejected(request, "process_snapshot_failed", &error.to_string()),
        };
        for process in &mut processes {
            if let Some(record) = self.executions.get(&process.process_id) {
                process.process_id = record.process_id.clone();
            }
        }
        DispatchResult {
            response: process_snapshot_response(
                request,
                processes
                    .into_iter()
                    .map(protocol_process_snapshot_entry)
                    .collect(),
            ),
            events: Vec::new(),
        }
    }

    fn get_signal_state(
        &mut self,
        request: &RequestFrame,
        payload: GetSignalStateRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "get_signal_state requires VM ownership",
            );
        };
        let Some(execution_id) = self.execution_id_for(&vm_id, &payload.process_id) else {
            return rejected(
                request,
                "unknown_process",
                "get_signal_state process is not active",
            );
        };
        let handlers = match self.sidecar.signal_state(&vm_id, &execution_id) {
            Ok(handlers) => handlers,
            Err(error) => return rejected(request, "signal_state_failed", &error.to_string()),
        };
        DispatchResult {
            response: signal_state_response(request, payload.process_id, handlers),
            events: Vec::new(),
        }
    }

    fn get_zombie_timer_count(
        &mut self,
        request: &RequestFrame,
        _payload: GetZombieTimerCountRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "get_zombie_timer_count requires VM ownership",
            );
        };
        let count = match self.sidecar.zombie_timer_count(&vm_id) {
            Ok(count) => count,
            Err(error) => {
                return rejected(request, "zombie_timer_count_failed", &error.to_string())
            }
        };
        DispatchResult {
            response: zombie_timer_count_response(request, count),
            events: Vec::new(),
        }
    }

    fn find_listener(
        &mut self,
        request: &RequestFrame,
        payload: FindListenerRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "find_listener requires VM ownership",
            );
        };
        let listener = match self.sidecar.find_listener(&vm_id, &payload) {
            Ok(listener) => listener,
            Err(error) => return rejected(request, "find_listener_failed", &error.to_string()),
        }
        .map(|entry| self.client_socket_state_entry(entry));
        DispatchResult {
            response: listener_snapshot_response(request, listener),
            events: Vec::new(),
        }
    }

    fn find_bound_udp(
        &mut self,
        request: &RequestFrame,
        payload: FindBoundUdpRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "find_bound_udp requires VM ownership",
            );
        };
        let socket = match self.sidecar.find_bound_udp(&vm_id, &payload) {
            Ok(socket) => socket,
            Err(error) => return rejected(request, "find_bound_udp_failed", &error.to_string()),
        }
        .map(|entry| self.client_socket_state_entry(entry));
        DispatchResult {
            response: bound_udp_snapshot_response(request, socket),
            events: Vec::new(),
        }
    }

    fn vm_fetch(&mut self, request: &RequestFrame, payload: VmFetchRequest) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "vm.fetch requires VM ownership",
            );
        };
        if let Err(error) = serde_json::from_str::<serde_json::Value>(&payload.headers_json) {
            return rejected(
                request,
                "invalid_request",
                &format!("vm.fetch headers_json must be valid JSON: {error}"),
            );
        }

        let response_json = match self.sidecar.vm_fetch(&vm_id, &payload) {
            Ok(response_json) => response_json,
            Err(error) => return rejected(request, "vm_fetch_failed", &error.to_string()),
        };
        DispatchResult {
            response: respond(
                request,
                ResponsePayload::VmFetchResult(VmFetchResponse { response_json }),
            ),
            events: Vec::new(),
        }
    }

    fn authenticate(
        &mut self,
        request: &RequestFrame,
        payload: AuthenticateRequest,
    ) -> DispatchResult {
        if let Err(error) = validate_authenticate_versions(&payload) {
            return rejected(request, error.code(), error.message());
        }

        self.next_connection += 1;
        let connection_id = format!("browser-connection-{}", self.next_connection);
        DispatchResult {
            response: authenticated_response(
                request.request_id,
                BROWSER_SIDECAR_ID,
                connection_id,
                BROWSER_MAX_FRAME_BYTES as u32,
            ),
            events: Vec::new(),
        }
    }

    fn client_socket_state_entry(&self, mut entry: SocketStateEntry) -> SocketStateEntry {
        if let Some(record) = self.executions.get(&entry.process_id) {
            entry.process_id = record.process_id.clone();
        }
        entry
    }

    fn open_session(
        &mut self,
        request: &RequestFrame,
        _payload: OpenSessionRequest,
    ) -> DispatchResult {
        let Some(connection_id) = connection_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "open_session requires connection ownership",
            );
        };
        self.next_session += 1;
        let session_id = format!("browser-session-{}", self.next_session);
        DispatchResult {
            response: session_opened_response(request.request_id, connection_id, session_id),
            events: Vec::new(),
        }
    }

    fn create_vm(&mut self, request: &RequestFrame, payload: CreateVmRequest) -> DispatchResult {
        let Some((connection_id, session_id)) = session_scope_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "create_vm requires session ownership",
            );
        };
        let create_config: CreateVmConfig = match serde_json::from_str(&payload.config) {
            Ok(config) => config,
            Err(error) => {
                return rejected(
                    request,
                    "invalid_config",
                    &format!("invalid create VM config JSON: {error}"),
                );
            }
        };
        if let Err(error) = create_config.validate(BROWSER_MAX_FRAME_BYTES) {
            return rejected(
                request,
                "invalid_config",
                &format!("invalid create VM config: {error}"),
            );
        }

        self.next_vm += 1;
        let vm_id = format!("vm-{}", self.next_vm);
        let mut kernel_config = KernelVmConfig::new(vm_id.clone());
        kernel_config.env = create_config.env.clone();
        if let Some(cwd) = create_config.cwd.clone() {
            kernel_config.cwd = cwd;
        }
        kernel_config.loopback_exempt_ports = create_config
            .loopback_exempt_ports
            .iter()
            .copied()
            .collect();
        let limits = match agentos_native_sidecar_core::vm_limits_from_config(
            create_config.limits.as_ref(),
            BROWSER_MAX_FRAME_BYTES,
        ) {
            Ok(limits) => limits,
            Err(error) => {
                return rejected(request, "invalid_config", &error.to_string());
            }
        };
        kernel_config.resources = limits.resources;
        let permissions = create_config
            .permissions
            .clone()
            .unwrap_or_else(agentos_native_sidecar_core::deny_all_policy);
        if let Err(error) = agentos_native_sidecar_core::validate_permissions_policy(&permissions) {
            return rejected(request, "invalid_config", &error.to_string());
        }
        kernel_config.permissions = permissions_from_policy(permissions);

        if let Err(error) = self
            .sidecar
            .create_vm_with_root_filesystem(kernel_config, create_config.root_filesystem)
        {
            return rejected(request, "create_vm_failed", &error.to_string());
        }
        self.active_vms.insert(vm_id.clone());

        let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
        DispatchResult {
            response: vm_created_response(request, vm_id.clone()),
            events: vec![
                vm_lifecycle_event(
                    &connection_id,
                    &session_id,
                    &vm_id,
                    VmLifecycleState::Creating,
                ),
                EventFrame::new(
                    ownership,
                    agentos_sidecar_protocol::protocol::EventPayload::VmLifecycle(
                        agentos_sidecar_protocol::protocol::VmLifecycleEvent {
                            state: VmLifecycleState::Ready,
                        },
                    ),
                ),
            ],
        }
    }

    fn dispose_vm(&mut self, request: &RequestFrame, _payload: DisposeVmRequest) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "dispose_vm requires VM ownership",
            );
        };
        if let Err(error) = self.sidecar.dispose_vm(&vm_id) {
            return rejected(request, "dispose_vm_failed", &error.to_string());
        }
        self.active_vms.remove(&vm_id);
        self.executions.retain(|_, record| record.vm_id != vm_id);
        self.process_executions
            .retain(|(process_vm_id, _), _| process_vm_id != &vm_id);
        DispatchResult {
            response: vm_disposed_response(request, vm_id),
            events: Vec::new(),
        }
    }

    fn ext(&mut self, request: &RequestFrame, payload: ExtEnvelope) -> DispatchResult {
        let response = match self
            .sidecar
            .dispatch_extension_request(BrowserExtensionRequest {
                namespace: payload.namespace,
                payload: payload.payload,
                vm_id: vm_id_of(&request.ownership),
                connection_id: connection_id_of(&request.ownership),
            }) {
            Ok(response) => response,
            Err(error) => return rejected(request, "extension_failed", &error.to_string()),
        };
        DispatchResult {
            response: respond(
                request,
                ResponsePayload::ExtResult(ExtEnvelope {
                    namespace: response.namespace,
                    payload: response.payload,
                }),
            ),
            events: Vec::new(),
        }
    }

    fn execute(&mut self, request: &RequestFrame, payload: ExecuteRequest) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "execute requires VM ownership",
            );
        };
        let process_key = (vm_id.clone(), payload.process_id.clone());
        if self.process_executions.contains_key(&process_key) {
            return rejected(
                request,
                "process_already_active",
                "process_id is already active",
            );
        }
        let runtime = match payload
            .runtime
            .clone()
            .unwrap_or(GuestRuntimeKind::JavaScript)
        {
            GuestRuntimeKind::JavaScript | GuestRuntimeKind::Python => GuestRuntime::JavaScript,
            GuestRuntimeKind::WebAssembly => GuestRuntime::WebAssembly,
        };
        let context = match runtime {
            GuestRuntime::JavaScript => {
                self.sidecar
                    .create_javascript_context(CreateJavascriptContextRequest {
                        vm_id: vm_id.clone(),
                        bootstrap_module: payload.entrypoint.clone(),
                    })
            }
            GuestRuntime::WebAssembly => {
                self.sidecar.create_wasm_context(CreateWasmContextRequest {
                    vm_id: vm_id.clone(),
                    module_path: payload.entrypoint.clone(),
                })
            }
        };
        let context = match context {
            Ok(context) => context,
            Err(error) => return rejected(request, "execute_failed", &error.to_string()),
        };

        let mut argv = Vec::new();
        if let Some(command) = payload.command.clone() {
            argv.push(command);
        }
        argv.extend(payload.args.clone());
        let started = match self.sidecar.start_execution_with_options(
            StartExecutionRequest {
                vm_id: vm_id.clone(),
                context_id: context.context_id,
                argv,
                env: payload.env.clone().into_iter().collect(),
                cwd: payload.cwd.clone().unwrap_or_else(|| String::from("/")),
            },
            BrowserExecutionOptions {
                command_name: payload.command.clone(),
                wasm_permission_tier: payload.wasm_permission_tier,
            },
        ) {
            Ok(started) => started,
            Err(error) => return rejected(request, "execute_failed", &error.to_string()),
        };

        self.executions.insert(
            started.execution_id.clone(),
            ExecutionRecord {
                vm_id: vm_id.clone(),
                process_id: payload.process_id.clone(),
                ownership: request.ownership.clone(),
            },
        );
        self.process_executions
            .insert(process_key, started.execution_id.clone());
        DispatchResult {
            response: process_started_response(request, payload.process_id, None),
            events: Vec::new(),
        }
    }

    fn write_stdin(
        &mut self,
        request: &RequestFrame,
        payload: WriteStdinRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "write_stdin requires VM ownership",
            );
        };
        let Some(execution_id) = self.execution_id_for(&vm_id, &payload.process_id) else {
            return rejected(
                request,
                "unknown_process",
                "write_stdin process is not active",
            );
        };
        let accepted_bytes = payload.chunk.len() as u64;
        if let Err(error) = self.sidecar.write_stdin(WriteExecutionStdinRequest {
            vm_id,
            execution_id,
            chunk: payload.chunk,
        }) {
            return rejected(request, "write_stdin_failed", &error.to_string());
        }
        DispatchResult {
            response: stdin_written_response(request, payload.process_id, accepted_bytes),
            events: Vec::new(),
        }
    }

    fn close_stdin(
        &mut self,
        request: &RequestFrame,
        payload: CloseStdinRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "close_stdin requires VM ownership",
            );
        };
        let Some(execution_id) = self.execution_id_for(&vm_id, &payload.process_id) else {
            return rejected(
                request,
                "unknown_process",
                "close_stdin process is not active",
            );
        };
        if let Err(error) = self.sidecar.close_stdin(ExecutionHandleRequest {
            vm_id,
            execution_id,
        }) {
            return rejected(request, "close_stdin_failed", &error.to_string());
        }
        DispatchResult {
            response: stdin_closed_response(request, payload.process_id),
            events: Vec::new(),
        }
    }

    fn kill_process(
        &mut self,
        request: &RequestFrame,
        payload: KillProcessRequest,
    ) -> DispatchResult {
        let Some(vm_id) = vm_id_of(&request.ownership) else {
            return rejected(
                request,
                "invalid_ownership",
                "kill_process requires VM ownership",
            );
        };
        let Some(execution_id) = self.execution_id_for(&vm_id, &payload.process_id) else {
            return DispatchResult {
                response: process_killed_response(request, payload.process_id),
                events: Vec::new(),
            };
        };
        let signal = match agentos_native_sidecar_core::parse_posix_signal(&payload.signal) {
            Some(signal) => signal,
            None => {
                return rejected(
                    request,
                    "kill_process_failed",
                    &format!("unsupported kill_process signal {}", payload.signal),
                );
            }
        };
        if signal == 0 {
            return DispatchResult {
                response: process_killed_response(request, payload.process_id),
                events: Vec::new(),
            };
        }
        if let Err(error) =
            self.sidecar
                .signal_execution_kernel_process(&vm_id, &execution_id, signal)
        {
            return rejected(request, "kill_process_failed", &error.to_string());
        }
        if let Some(bridge_signal) = execution_signal_from_number(signal) {
            if let Err(error) = self
                .sidecar
                .bridge_mut()
                .kill_execution(KillExecutionRequest {
                    vm_id,
                    execution_id,
                    signal: bridge_signal,
                })
            {
                return rejected(request, "kill_process_failed", &format!("{error:?}"));
            }
        }
        DispatchResult {
            response: process_killed_response(request, payload.process_id),
            events: Vec::new(),
        }
    }

    fn pump_execution_events(&mut self) {
        for vm_id in self.active_vms.iter().cloned().collect::<Vec<_>>() {
            while let Ok(Some(event)) =
                self.sidecar
                    .poll_execution_event(PollExecutionEventRequest {
                        vm_id: vm_id.clone(),
                    })
            {
                if let Some(frame) = self.execution_event_to_frame(event) {
                    self.pending_events.push_back(frame);
                }
            }
        }
    }

    fn execution_event_to_frame(&mut self, event: ExecutionEvent) -> Option<EventFrame> {
        match event {
            ExecutionEvent::Stdout(chunk) => {
                let record = self.executions.get(&chunk.execution_id)?;
                Some(process_output_event(
                    record.ownership.clone(),
                    &record.process_id,
                    StreamChannel::Stdout,
                    chunk.chunk,
                ))
            }
            ExecutionEvent::Stderr(chunk) => {
                let record = self.executions.get(&chunk.execution_id)?;
                Some(process_output_event(
                    record.ownership.clone(),
                    &record.process_id,
                    StreamChannel::Stderr,
                    chunk.chunk,
                ))
            }
            ExecutionEvent::Exited(exited) => {
                let record = self.executions.remove(&exited.execution_id)?;
                self.process_executions
                    .remove(&(record.vm_id.clone(), record.process_id.clone()));
                Some(process_exited_event(
                    record.ownership,
                    &record.process_id,
                    exited.exit_code,
                ))
            }
            ExecutionEvent::GuestRequest(call) => {
                let record = self.executions.get(&call.execution_id)?;
                Some(unsupported_guest_kernel_call_event(
                    record.ownership.clone(),
                    &record.process_id,
                    &call.execution_id,
                    &call.operation,
                    call.payload.len(),
                ))
            }
            ExecutionEvent::SignalState(_) => None,
        }
    }

    fn execution_id_for(&self, vm_id: &str, process_id: &str) -> Option<String> {
        self.process_executions
            .get(&(vm_id.to_string(), process_id.to_string()))
            .cloned()
    }
}

fn rejected(request: &RequestFrame, code: &str, message: &str) -> DispatchResult {
    DispatchResult {
        response: reject(request, code, message),
        events: Vec::new(),
    }
}
