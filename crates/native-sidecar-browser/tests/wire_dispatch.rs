#[path = "../../bridge/tests/support.rs"]
mod bridge_support;

use agentos_bridge::{
    CreateJavascriptContextRequest, ExecutionEvent, ExecutionSignal, ExecutionSignalState,
    GuestKernelCall, OutputChunk, SignalDispositionAction, SignalHandlerRegistration,
    StartExecutionRequest,
};
use agentos_kernel::kernel::KernelVmConfig;
use agentos_kernel::permissions::Permissions;
use agentos_native_sidecar_browser::{
    wire_dispatch::BrowserWireDispatcher, BrowserExtension, BrowserExtensionContext,
    BrowserSidecarError, BrowserWorkerBridge, BrowserWorkerHandle, BrowserWorkerHandleRequest,
    BrowserWorkerSpawnRequest,
};
use agentos_sidecar_protocol::wire::{
    protocol_schema, AuthenticateRequest, BootstrapRootFilesystemRequest, ConfigureVmRequest,
    ConnectionOwnership, CreateOverlayRequest, CreateVmRequest, ExecuteRequest,
    ExportSnapshotRequest, ExtEnvelope, FilesystemOperation, FindBoundUdpRequest,
    FindListenerRequest, GetSignalStateRequest, GuestFilesystemCallRequest,
    GuestFilesystemOperation, GuestRuntimeKind, HostFilesystemCallRequest, ImportSnapshotRequest,
    KillProcessRequest, OpenSessionRequest, OwnershipScope, PermissionsPolicy,
    PersistenceFlushRequest, PersistenceLoadRequest, ProtocolFrame, RegisterHostCallbacksRequest,
    RegisteredHostCallbackDefinition, RequestFrame, RequestPayload, ResponsePayload,
    RootFilesystemEntry, RootFilesystemEntryEncoding, RootFilesystemEntryKind, RootFilesystemMode,
    SealLayerRequest, SidecarPlacement, SidecarPlacementShared, VmFetchRequest, VmOwnership,
    WasmPermissionTier, WireFrameCodec, PROTOCOL_VERSION,
};
use bridge_support::RecordingBridge;
use std::collections::{BTreeMap, HashMap};

struct WireExtension;

impl BrowserExtension for WireExtension {
    fn namespace(&self) -> &str {
        "dev.rivet.agentos.browser-wire-test"
    }

    fn handle_request(
        &self,
        _context: &mut BrowserExtensionContext<'_>,
        payload: &[u8],
    ) -> Result<Vec<u8>, BrowserSidecarError> {
        let mut response = b"wire-ext:".to_vec();
        response.extend_from_slice(payload);
        Ok(response)
    }
}

impl BrowserWorkerBridge for RecordingBridge {
    fn create_worker(
        &mut self,
        request: BrowserWorkerSpawnRequest,
    ) -> Result<BrowserWorkerHandle, Self::Error> {
        self.browser_worker_spawns.push(BTreeMap::from([(
            String::from("wasm_permission_tier"),
            request
                .wasm_permission_tier
                .map(|tier| format!("{tier:?}"))
                .unwrap_or_default(),
        )]));
        Ok(BrowserWorkerHandle {
            worker_id: format!("wire-worker-{}", request.context_id),
            runtime: request.runtime,
        })
    }

    fn terminate_worker(
        &mut self,
        _request: BrowserWorkerHandleRequest,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn create_wire_vm(
    codec: &WireFrameCodec,
    dispatcher: &mut BrowserWireDispatcher<RecordingBridge>,
) -> (String, OwnershipScope) {
    let auth = dispatch(
        codec,
        dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        },
    );
    let ResponsePayload::AuthenticatedResponse(authenticated) = auth.payload else {
        panic!("unexpected auth response: {:?}", auth.payload);
    };
    let connection = OwnershipScope::ConnectionOwnership(ConnectionOwnership {
        connection_id: authenticated.connection_id.clone(),
    });
    let session = dispatch(
        codec,
        dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: connection,
            payload: RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: Default::default(),
            }),
        },
    );
    let ResponsePayload::SessionOpenedResponse(opened) = session.payload else {
        panic!("unexpected session response: {:?}", session.payload);
    };
    let config = agentos_vm_config::CreateVmConfig {
        cwd: Some(String::from("/workspace")),
        permissions: Some(agentos_native_sidecar_core::allow_all_policy()),
        ..Default::default()
    };
    let created = dispatch(
        codec,
        dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership: OwnershipScope::SessionOwnership(
                agentos_sidecar_protocol::wire::SessionOwnership {
                    connection_id: authenticated.connection_id.clone(),
                    session_id: opened.session_id.clone(),
                },
            ),
            payload: RequestPayload::CreateVmRequest(CreateVmRequest::json_config(
                GuestRuntimeKind::JavaScript,
                config,
            )),
        },
    );
    let ResponsePayload::VmCreatedResponse(created) = created.payload else {
        panic!("unexpected create VM response: {:?}", created.payload);
    };
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: authenticated.connection_id,
        session_id: opened.session_id,
        vm_id: created.vm_id.clone(),
    });
    (created.vm_id, ownership)
}

fn execute_wire_process(
    codec: &WireFrameCodec,
    dispatcher: &mut BrowserWireDispatcher<RecordingBridge>,
    ownership: OwnershipScope,
    request_id: i64,
    process_id: &str,
) -> ResponsePayload {
    dispatch(
        codec,
        dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id,
            ownership,
            payload: RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_string(),
                command: Some(String::from("node")),
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(String::from("/workspace/main.js")),
                args: vec![String::from("main.js")],
                env: Default::default(),
                cwd: Some(String::from("/workspace")),
                wasm_permission_tier: None,
            }),
        },
    )
    .payload
}

#[test]
fn browser_wire_dispatcher_handles_lifecycle_and_execution_frames() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

    let auth = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        },
    );
    let ResponsePayload::AuthenticatedResponse(authenticated) = auth.payload else {
        panic!("unexpected auth response: {:?}", auth.payload);
    };
    assert_eq!(authenticated.sidecar_id, "agentos-native-sidecar-browser");

    let session = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: authenticated.connection_id.clone(),
            }),
            payload: RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: Default::default(),
            }),
        },
    );
    let ResponsePayload::SessionOpenedResponse(opened) = session.payload else {
        panic!("unexpected session response: {:?}", session.payload);
    };

    let mut config = agentos_vm_config::CreateVmConfig {
        cwd: Some(String::from("/workspace")),
        permissions: Some(agentos_native_sidecar_core::allow_all_policy()),
        ..Default::default()
    };
    config
        .env
        .insert(String::from("BASE_ENV"), String::from("base"));
    let create = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership: OwnershipScope::SessionOwnership(
                agentos_sidecar_protocol::wire::SessionOwnership {
                    connection_id: authenticated.connection_id.clone(),
                    session_id: opened.session_id.clone(),
                },
            ),
            payload: RequestPayload::CreateVmRequest(CreateVmRequest::json_config(
                GuestRuntimeKind::JavaScript,
                config,
            )),
        },
    );
    let ResponsePayload::VmCreatedResponse(created) = create.payload else {
        panic!("unexpected create response: {:?}", create.payload);
    };
    assert_eq!(dispatcher.vm_count(), 1);

    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: authenticated.connection_id,
        session_id: opened.session_id,
        vm_id: created.vm_id.clone(),
    });

    let bootstrap = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 4,
            ownership: ownership.clone(),
            payload: RequestPayload::BootstrapRootFilesystemRequest(
                BootstrapRootFilesystemRequest {
                    entries: vec![RootFilesystemEntry {
                        path: String::from("/workspace/wire.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: Some(0o644),
                        uid: Some(1000),
                        gid: Some(1000),
                        content: Some(String::from("aGVsbG8gd2lyZQ==")),
                        encoding: Some(RootFilesystemEntryEncoding::Base64),
                        target: None,
                        executable: false,
                    }],
                },
            ),
        },
    );
    assert!(matches!(
        bootstrap.payload,
        ResponsePayload::RootFilesystemBootstrappedResponse(_)
    ));

    let read_file = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 5,
            ownership: ownership.clone(),
            payload: RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/wire.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
                max_depth: None,
            }),
        },
    );
    let ResponsePayload::GuestFilesystemResultResponse(result) = read_file.payload else {
        panic!("unexpected read_file response: {:?}", read_file.payload);
    };
    assert_eq!(result.content.as_deref(), Some("hello wire"));
    assert_eq!(result.encoding, Some(RootFilesystemEntryEncoding::Utf8));

    let snapshot = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 6,
            ownership: ownership.clone(),
            payload: RequestPayload::SnapshotRootFilesystemRequest,
        },
    );
    let ResponsePayload::RootFilesystemSnapshotResponse(snapshot) = snapshot.payload else {
        panic!("unexpected snapshot response: {:?}", snapshot.payload);
    };
    assert!(snapshot
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/wire.txt"
            && entry.content.as_deref() == Some("hello wire")));

    let execute = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 7,
            ownership: ownership.clone(),
            payload: RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: String::from("proc-1"),
                command: Some(String::from("node")),
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(String::from("/workspace/main.js")),
                args: vec![String::from("main.js")],
                env: Default::default(),
                cwd: Some(String::from("/workspace")),
                wasm_permission_tier: None,
            }),
        },
    );
    assert!(matches!(
        execute.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));

    dispatcher
        .sidecar_mut()
        .create_kernel_tcp_listener_for_execution(&created.vm_id, "exec-1", "127.0.0.1", 34567, 16)
        .expect("create kernel listener");
    dispatcher
        .sidecar_mut()
        .create_kernel_bound_udp_for_execution(&created.vm_id, "exec-1", "127.0.0.1", 34568)
        .expect("create kernel UDP socket");

    let listener = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 71,
            ownership: ownership.clone(),
            payload: RequestPayload::FindListenerRequest(FindListenerRequest {
                host: Some(String::from("localhost")),
                port: Some(34567),
                path: None,
            }),
        },
    );
    let ResponsePayload::ListenerSnapshotResponse(listener) = listener.payload else {
        panic!("unexpected listener response: {:?}", listener.payload);
    };
    let listener = listener.listener.expect("listener should be found");
    assert_eq!(listener.process_id, "proc-1");
    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(listener.port, Some(34567));

    let udp = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 72,
            ownership: ownership.clone(),
            payload: RequestPayload::FindBoundUdpRequest(FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(34568),
            }),
        },
    );
    let ResponsePayload::BoundUdpSnapshotResponse(udp) = udp.payload else {
        panic!("unexpected UDP response: {:?}", udp.payload);
    };
    let udp = udp.socket.expect("UDP socket should be found");
    assert_eq!(udp.process_id, "proc-1");
    assert_eq!(udp.host.as_deref(), Some("127.0.0.1"));
    assert_eq!(udp.port, Some(34568));

    let snapshot = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 8,
            ownership: ownership.clone(),
            payload: RequestPayload::GetProcessSnapshotRequest,
        },
    );
    let ResponsePayload::ProcessSnapshotResponse(snapshot) = snapshot.payload else {
        panic!(
            "unexpected process snapshot response: {:?}",
            snapshot.payload
        );
    };
    let process = snapshot
        .processes
        .iter()
        .find(|process| process.process_id == "proc-1")
        .expect("client process should be represented in snapshot");
    assert!(process.pid > 0);
    assert_eq!(process.cwd, "/workspace");

    dispatcher
        .sidecar_mut()
        .bridge_mut()
        .push_execution_event(ExecutionEvent::SignalState(ExecutionSignalState {
            vm_id: created.vm_id.clone(),
            execution_id: String::from("exec-1"),
            signal: 15,
            registration: SignalHandlerRegistration {
                action: SignalDispositionAction::User,
                mask: vec![2],
                flags: 0,
            },
        }));
    while dispatcher
        .poll_event_bytes()
        .expect("pump signal state")
        .is_some()
    {}

    let signal_state = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 9,
            ownership: ownership.clone(),
            payload: RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                process_id: String::from("proc-1"),
            }),
        },
    );
    let ResponsePayload::SignalStateResponse(signal_state) = signal_state.payload else {
        panic!(
            "unexpected signal state response: {:?}",
            signal_state.payload
        );
    };
    assert_eq!(signal_state.process_id, "proc-1");
    let sigterm = signal_state.handlers.get(&15).expect("SIGTERM handler");
    assert_eq!(
        sigterm.action,
        agentos_sidecar_protocol::wire::SignalDispositionAction::User
    );
    assert_eq!(sigterm.mask, vec![2]);

    let invalid_signal = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 91,
            ownership: ownership.clone(),
            payload: RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-1"),
                signal: String::from("SIGBOGUS"),
            }),
        },
    );
    let ResponsePayload::RejectedResponse(rejected) = invalid_signal.payload else {
        panic!(
            "unexpected invalid signal response: {:?}",
            invalid_signal.payload
        );
    };
    assert_eq!(rejected.code, "kill_process_failed");
    assert!(rejected
        .message
        .contains("unsupported kill_process signal SIGBOGUS"));
    assert!(dispatcher
        .sidecar_mut()
        .bridge()
        .killed_executions
        .is_empty());

    let signal_zero = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 92,
            ownership: ownership.clone(),
            payload: RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-1"),
                signal: String::from("0"),
            }),
        },
    );
    assert!(matches!(
        signal_zero.payload,
        ResponsePayload::ProcessKilledResponse(_)
    ));
    assert!(dispatcher
        .sidecar_mut()
        .bridge()
        .killed_executions
        .is_empty());

    let continue_signal = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 94,
            ownership: ownership.clone(),
            payload: RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-1"),
                signal: String::from("SIGCONT"),
            }),
        },
    );
    assert!(matches!(
        continue_signal.payload,
        ResponsePayload::ProcessKilledResponse(_)
    ));
    assert!(dispatcher
        .sidecar_mut()
        .bridge()
        .killed_executions
        .is_empty());

    let zombie_count = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 10,
            ownership: ownership.clone(),
            payload: RequestPayload::GetZombieTimerCountRequest,
        },
    );
    let ResponsePayload::ZombieTimerCountResponse(zombie_count) = zombie_count.payload else {
        panic!(
            "unexpected zombie count response: {:?}",
            zombie_count.payload
        );
    };
    assert_eq!(zombie_count.count, 0);

    dispatcher
        .sidecar_mut()
        .bridge_mut()
        .push_execution_event(ExecutionEvent::Stdout(OutputChunk {
            vm_id: created.vm_id,
            execution_id: String::from("exec-1"),
            chunk: b"hello\n".to_vec(),
        }));
    let output = loop {
        let event = dispatcher
            .poll_event_bytes()
            .expect("poll event")
            .expect("event should be encoded");
        let ProtocolFrame::EventFrame(event) = codec.decode_message(&event).expect("decode event")
        else {
            panic!("expected event frame");
        };
        if let agentos_sidecar_protocol::wire::EventPayload::ProcessOutputEvent(output) =
            event.payload
        {
            break output;
        }
    };
    assert_eq!(output.process_id, "proc-1");
    assert_eq!(output.chunk, b"hello\n");

    dispatcher
        .sidecar_mut()
        .bridge_mut()
        .push_execution_event(ExecutionEvent::GuestRequest(GuestKernelCall {
            vm_id: String::from("vm-1"),
            execution_id: String::from("exec-1"),
            operation: String::from("fs.read"),
            payload: b"{\"path\":\"/workspace/input.txt\"}".to_vec(),
        }));
    let guest_request_event = loop {
        let event = dispatcher
            .poll_event_bytes()
            .expect("poll guest request event")
            .expect("guest request event should be encoded");
        let ProtocolFrame::EventFrame(event) = codec.decode_message(&event).expect("decode event")
        else {
            panic!("expected event frame");
        };
        if let agentos_sidecar_protocol::wire::EventPayload::StructuredEvent(event) = event.payload
        {
            if event.name == "guest.kernel_call.unsupported" {
                break event;
            }
        }
    };
    assert_eq!(guest_request_event.detail["process_id"], "proc-1");
    assert_eq!(guest_request_event.detail["execution_id"], "exec-1");
    assert_eq!(guest_request_event.detail["operation"], "fs.read");
    assert_eq!(
        guest_request_event.detail["payload_size_bytes"],
        b"{\"path\":\"/workspace/input.txt\"}".len().to_string()
    );

    let interrupt = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 93,
            ownership,
            payload: RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-1"),
                signal: String::from("SIGINT"),
            }),
        },
    );
    assert!(matches!(
        interrupt.payload,
        ResponsePayload::ProcessKilledResponse(_)
    ));
    let killed = &dispatcher.sidecar_mut().bridge().killed_executions;
    assert_eq!(killed.len(), 1);
    assert_eq!(killed[0].signal, ExecutionSignal::Interrupt);
}

#[test]
fn browser_wire_dispatcher_rejects_duplicate_active_process_ids() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let (_, ownership) = create_wire_vm(&codec, &mut dispatcher);

    let first = execute_wire_process(&codec, &mut dispatcher, ownership.clone(), 10, "proc-1");
    assert!(matches!(first, ResponsePayload::ProcessStartedResponse(_)));

    let duplicate = execute_wire_process(&codec, &mut dispatcher, ownership, 11, "proc-1");
    let ResponsePayload::RejectedResponse(rejected) = duplicate else {
        panic!("unexpected duplicate process response: {:?}", duplicate);
    };
    assert_eq!(rejected.code, "process_already_active");
    assert_eq!(
        dispatcher
            .sidecar_mut()
            .bridge()
            .browser_worker_spawns
            .len(),
        1
    );
}

#[test]
fn browser_wire_dispatcher_rejects_protocol_version_mismatch() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION + 1,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        },
    );

    let ResponsePayload::RejectedResponse(rejected) = response.payload else {
        panic!("unexpected auth mismatch response: {:?}", response.payload);
    };
    assert_eq!(rejected.code, "protocol_version_mismatch");
    assert!(rejected
        .message
        .contains("sidecar protocol version mismatch"));
}

#[test]
fn browser_wire_dispatcher_rejects_bridge_contract_version_mismatch() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version + 1,
            }),
        },
    );

    let ResponsePayload::RejectedResponse(rejected) = response.payload else {
        panic!(
            "unexpected bridge mismatch response: {:?}",
            response.payload
        );
    };
    assert_eq!(rejected.code, "bridge_version_mismatch");
    assert!(rejected
        .message
        .contains("bridge contract version mismatch"));
}

#[test]
fn browser_wire_dispatcher_routes_extension_frames() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    dispatcher
        .sidecar_mut()
        .register_extension(Box::new(WireExtension))
        .expect("register wire extension");

    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from("dev.rivet.agentos.browser-wire-test"),
                payload: b"ping".to_vec(),
            }),
        },
    );

    let ResponsePayload::ExtEnvelope(envelope) = response.payload else {
        panic!("unexpected extension response: {:?}", response.payload);
    };
    assert_eq!(envelope.namespace, "dev.rivet.agentos.browser-wire-test");
    assert_eq!(envelope.payload, b"wire-ext:ping");
}

#[test]
fn browser_wire_dispatcher_configures_vm_permissions() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm-config");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create configurable vm");
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: String::from("conn"),
        session_id: String::from("session"),
        vm_id: String::from("vm-config"),
    });

    let bootstrap = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: ownership.clone(),
            payload: RequestPayload::BootstrapRootFilesystemRequest(
                BootstrapRootFilesystemRequest {
                    entries: vec![RootFilesystemEntry {
                        path: String::from("/workspace/config.txt"),
                        kind: RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(String::from("before")),
                        encoding: Some(RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    }],
                },
            ),
        },
    );
    assert!(matches!(
        bootstrap.payload,
        ResponsePayload::RootFilesystemBootstrappedResponse(_)
    ));

    let configured = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: ownership.clone(),
            payload: RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy::deny_all()),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        },
    );
    let ResponsePayload::VmConfiguredResponse(configured) = configured.payload else {
        panic!("unexpected configure response: {:?}", configured.payload);
    };
    assert_eq!(configured.applied_mounts, 0);
    assert_eq!(configured.applied_software, 0);

    let read_after_deny = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership,
            payload: RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/config.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
                max_depth: None,
            }),
        },
    );
    let ResponsePayload::RejectedResponse(rejected) = read_after_deny.payload else {
        panic!(
            "unexpected read-after-configure response: {:?}",
            read_after_deny.payload
        );
    };
    assert_eq!(rejected.code, "guest_filesystem_failed");
}

#[test]
fn browser_wire_create_vm_without_permissions_defaults_to_deny_all() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

    let auth = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        },
    );
    let ResponsePayload::AuthenticatedResponse(authenticated) = auth.payload else {
        panic!("unexpected auth response: {:?}", auth.payload);
    };
    let session = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: authenticated.connection_id.clone(),
            }),
            payload: RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: Default::default(),
            }),
        },
    );
    let ResponsePayload::SessionOpenedResponse(opened) = session.payload else {
        panic!("unexpected session response: {:?}", session.payload);
    };

    let config = agentos_vm_config::CreateVmConfig {
        cwd: Some(String::from("/workspace")),
        permissions: None,
        root_filesystem: agentos_vm_config::RootFilesystemConfig {
            bootstrap_entries: vec![agentos_vm_config::RootFilesystemEntry {
                path: String::from("/workspace/default-deny.txt"),
                kind: agentos_vm_config::RootFilesystemEntryKind::File,
                mode: None,
                uid: None,
                gid: None,
                content: Some(String::from("secret")),
                encoding: Some(agentos_vm_config::RootFilesystemEntryEncoding::Utf8),
                target: None,
                executable: false,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let create = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership: OwnershipScope::SessionOwnership(
                agentos_sidecar_protocol::wire::SessionOwnership {
                    connection_id: authenticated.connection_id.clone(),
                    session_id: opened.session_id.clone(),
                },
            ),
            payload: RequestPayload::CreateVmRequest(CreateVmRequest::json_config(
                GuestRuntimeKind::JavaScript,
                config,
            )),
        },
    );
    let ResponsePayload::VmCreatedResponse(created) = create.payload else {
        panic!("unexpected create response: {:?}", create.payload);
    };

    let read = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 4,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: authenticated.connection_id,
                session_id: opened.session_id,
                vm_id: created.vm_id,
            }),
            payload: RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/default-deny.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
                max_depth: None,
            }),
        },
    );
    let ResponsePayload::RejectedResponse(rejected) = read.payload else {
        panic!("unexpected default-deny read response: {:?}", read.payload);
    };
    assert_eq!(rejected.code, "guest_filesystem_failed");
}

#[test]
fn browser_wire_create_vm_applies_read_only_root_filesystem_config() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

    let auth = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("browser-wire-test"),
                auth_token: String::from("test-token"),
                protocol_version: PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        },
    );
    let ResponsePayload::AuthenticatedResponse(authenticated) = auth.payload else {
        panic!("unexpected auth response: {:?}", auth.payload);
    };
    let session = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: authenticated.connection_id.clone(),
            }),
            payload: RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: Default::default(),
            }),
        },
    );
    let ResponsePayload::SessionOpenedResponse(opened) = session.payload else {
        panic!("unexpected session response: {:?}", session.payload);
    };

    let config = agentos_vm_config::CreateVmConfig {
        cwd: Some(String::from("/workspace")),
        permissions: Some(agentos_native_sidecar_core::allow_all_policy()),
        root_filesystem: agentos_vm_config::RootFilesystemConfig {
            mode: agentos_vm_config::RootFilesystemMode::ReadOnly,
            disable_default_base_layer: true,
            bootstrap_entries: vec![agentos_vm_config::RootFilesystemEntry {
                path: String::from("/workspace/bootstrap.txt"),
                kind: agentos_vm_config::RootFilesystemEntryKind::File,
                mode: None,
                uid: None,
                gid: None,
                content: Some(String::from("bootstrapped")),
                encoding: Some(agentos_vm_config::RootFilesystemEntryEncoding::Utf8),
                target: None,
                executable: false,
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let create = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership: OwnershipScope::SessionOwnership(
                agentos_sidecar_protocol::wire::SessionOwnership {
                    connection_id: authenticated.connection_id.clone(),
                    session_id: opened.session_id.clone(),
                },
            ),
            payload: RequestPayload::CreateVmRequest(CreateVmRequest::json_config(
                GuestRuntimeKind::JavaScript,
                config,
            )),
        },
    );
    let ResponsePayload::VmCreatedResponse(created) = create.payload else {
        panic!("unexpected create response: {:?}", create.payload);
    };
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: authenticated.connection_id,
        session_id: opened.session_id,
        vm_id: created.vm_id,
    });

    let read = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 4,
            ownership: ownership.clone(),
            payload: RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/bootstrap.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
                max_depth: None,
            }),
        },
    );
    let ResponsePayload::GuestFilesystemResultResponse(read) = read.payload else {
        panic!("unexpected read response: {:?}", read.payload);
    };
    assert_eq!(read.content.as_deref(), Some("bootstrapped"));

    let write = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 5,
            ownership,
            payload: RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/new.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("new")),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
                max_depth: None,
            }),
        },
    );
    let ResponsePayload::RejectedResponse(rejected) = write.payload else {
        panic!("unexpected write response: {:?}", write.payload);
    };
    assert_eq!(rejected.code, "guest_filesystem_failed");
    assert_eq!(
        rejected.message,
        "EROFS: read-only filesystem: /workspace/new.txt"
    );
}

#[test]
fn browser_wire_dispatcher_configures_wasm_command_permissions() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm-wasm-perms");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create wasm permissions vm");
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: String::from("conn"),
        session_id: String::from("session"),
        vm_id: String::from("vm-wasm-perms"),
    });

    let configured = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: ownership.clone(),
            payload: RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: vec![String::from("keep wasm read-only")],
                projected_modules: Vec::new(),
                command_permissions: HashMap::from([(
                    String::from("wasm"),
                    WasmPermissionTier::ReadOnly,
                )]),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        },
    );
    assert!(matches!(
        configured.payload,
        ResponsePayload::VmConfiguredResponse(_)
    ));

    let executed = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership: ownership.clone(),
            payload: RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: String::from("proc-wasm"),
                command: Some(String::from("wasm")),
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(String::from("/workspace/app.wasm")),
                args: vec![String::from("/workspace/app.wasm")],
                env: Default::default(),
                cwd: Some(String::from("/workspace")),
                wasm_permission_tier: None,
            }),
        },
    );
    assert!(matches!(
        executed.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));
    assert_eq!(
        dispatcher
            .sidecar_mut()
            .bridge()
            .browser_worker_spawns
            .first()
            .and_then(|spawn| spawn.get("wasm_permission_tier"))
            .map(String::as_str),
        Some("ReadOnly")
    );

    let explicit = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership,
            payload: RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: String::from("proc-wasm-explicit"),
                command: Some(String::from("wasm")),
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(String::from("/workspace/app.wasm")),
                args: vec![String::from("/workspace/app.wasm")],
                env: Default::default(),
                cwd: Some(String::from("/workspace")),
                wasm_permission_tier: Some(WasmPermissionTier::ReadWrite),
            }),
        },
    );
    assert!(matches!(
        explicit.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));
    assert_eq!(
        dispatcher
            .sidecar_mut()
            .bridge()
            .browser_worker_spawns
            .get(1)
            .and_then(|spawn| spawn.get("wasm_permission_tier"))
            .map(String::as_str),
        Some("ReadWrite")
    );
}

#[test]
fn browser_wire_dispatcher_registers_host_callbacks() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm-bindings");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create bindings vm");
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: String::from("conn"),
        session_id: String::from("session"),
        vm_id: String::from("vm-bindings"),
    });

    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: ownership.clone(),
            payload: RequestPayload::RegisterHostCallbacksRequest(test_bindings_payload(
                "browser",
                "agentos-browser",
            )),
        },
    );
    let ResponsePayload::HostCallbacksRegisteredResponse(registered) = response.payload else {
        panic!("unexpected register response: {:?}", response.payload);
    };
    assert_eq!(registered.registration, "browser");
    assert_eq!(registered.command_count, 2);

    let duplicate = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 2,
            ownership,
            payload: RequestPayload::RegisterHostCallbacksRequest(test_bindings_payload(
                "browser",
                "agentos-browser-2",
            )),
        },
    );
    let ResponsePayload::RejectedResponse(rejected) = duplicate.payload else {
        panic!("unexpected duplicate response: {:?}", duplicate.payload);
    };
    assert_eq!(rejected.code, "register_host_callbacks_failed");
    assert!(rejected.message.contains("collection already registered"));
}

#[test]
fn browser_wire_dispatcher_manages_snapshot_layers() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm-layers");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create layer vm");
    let ownership = OwnershipScope::VmOwnership(VmOwnership {
        connection_id: String::from("conn"),
        session_id: String::from("session"),
        vm_id: String::from("vm-layers"),
    });

    let lower_layer = import_snapshot_layer(
        &codec,
        &mut dispatcher,
        ownership.clone(),
        1,
        &[
            ("/workspace/lower.txt", "lower"),
            ("/workspace/shared.txt", "lower"),
        ],
    );
    let upper_layer = import_snapshot_layer(
        &codec,
        &mut dispatcher,
        ownership.clone(),
        2,
        &[
            ("/workspace/upper.txt", "upper"),
            ("/workspace/shared.txt", "upper"),
        ],
    );

    let overlay = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 3,
            ownership: ownership.clone(),
            payload: RequestPayload::CreateOverlayRequest(CreateOverlayRequest {
                mode: RootFilesystemMode::Ephemeral,
                upper_layer_id: Some(upper_layer),
                lower_layer_ids: vec![lower_layer],
            }),
        },
    );
    let ResponsePayload::OverlayCreatedResponse(overlay) = overlay.payload else {
        panic!("unexpected overlay response: {:?}", overlay.payload);
    };

    let exported = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 4,
            ownership: ownership.clone(),
            payload: RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: overlay.layer_id,
            }),
        },
    );
    let ResponsePayload::SnapshotExportedResponse(exported) = exported.payload else {
        panic!("unexpected export response: {:?}", exported.payload);
    };
    assert!(exported
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/lower.txt"
            && entry.content.as_deref() == Some("lower")));
    assert!(exported
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/upper.txt"
            && entry.content.as_deref() == Some("upper")));
    assert!(exported
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/shared.txt"
            && entry.content.as_deref() == Some("upper")));

    let created = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 5,
            ownership: ownership.clone(),
            payload: RequestPayload::CreateLayerRequest,
        },
    );
    let ResponsePayload::LayerCreatedResponse(created) = created.payload else {
        panic!("unexpected create layer response: {:?}", created.payload);
    };
    let sealed = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 6,
            ownership,
            payload: RequestPayload::SealLayerRequest(SealLayerRequest {
                layer_id: created.layer_id,
            }),
        },
    );
    assert!(matches!(
        sealed.payload,
        ResponsePayload::LayerSealedResponse(_)
    ));
}

#[test]
fn browser_wire_dispatcher_rejects_reverse_host_callback_requests() {
    let codec = WireFrameCodec::default();
    for payload in [
        RequestPayload::HostFilesystemCallRequest(HostFilesystemCallRequest {
            operation: FilesystemOperation::Read,
            path: String::from("/state"),
            payload_size_bytes: 0,
        }),
        RequestPayload::PersistenceLoadRequest(PersistenceLoadRequest {
            key: String::from("state"),
        }),
        RequestPayload::PersistenceFlushRequest(PersistenceFlushRequest {
            key: String::from("state"),
            payload_size_bytes: 0,
        }),
    ] {
        let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());

        let response = dispatch(
            &codec,
            &mut dispatcher,
            RequestFrame {
                schema: protocol_schema(),
                request_id: 1,
                ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                    connection_id: String::from("client"),
                }),
                payload,
            },
        );

        let ResponsePayload::RejectedResponse(rejected) = response.payload else {
            panic!("unexpected rejection response: {:?}", response.payload);
        };
        assert_eq!(rejected.code, "unsupported_direction");
    }
}

#[test]
fn browser_wire_dispatcher_rejects_vm_fetch_when_guest_listener_is_missing() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create vm for vm.fetch");
    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: String::from("conn"),
                session_id: String::from("session"),
                vm_id: String::from("vm"),
            }),
            payload: RequestPayload::VmFetchRequest(VmFetchRequest {
                port: 3000,
                method: String::from("GET"),
                path: String::from("/"),
                headers_json: String::from("{}"),
                body: None,
                body_base64: None,
                stream_operation: None,
                stream_id: None,
                max_bytes: None,
            }),
        },
    );

    let ResponsePayload::RejectedResponse(rejected) = response.payload else {
        panic!("unexpected vm.fetch response: {:?}", response.payload);
    };
    assert_eq!(rejected.code, "vm_fetch_failed");
    assert!(
        rejected
            .message
            .contains("could not find a guest HTTP listener on port 3000"),
        "unexpected vm.fetch rejection: {}",
        rejected.message
    );
}

#[test]
fn browser_wire_dispatcher_rejects_vm_fetch_with_invalid_headers_json() {
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create vm for vm.fetch");
    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: String::from("conn"),
                session_id: String::from("session"),
                vm_id: String::from("vm"),
            }),
            payload: RequestPayload::VmFetchRequest(VmFetchRequest {
                port: 3000,
                method: String::from("GET"),
                path: String::from("/"),
                headers_json: String::from("{not-json"),
                body: None,
                body_base64: None,
                stream_operation: None,
                stream_id: None,
                max_bytes: None,
            }),
        },
    );

    let ResponsePayload::RejectedResponse(rejected) = response.payload else {
        panic!("unexpected vm.fetch response: {:?}", response.payload);
    };
    assert_eq!(rejected.code, "invalid_request");
    assert!(
        rejected.message.contains("headers_json must be valid JSON"),
        "unexpected vm.fetch rejection: {}",
        rejected.message
    );
}

#[test]
fn browser_wire_dispatcher_vm_fetch_enters_kernel_loopback_when_listener_exists() {
    std::env::set_var("AGENTOS_TEST_BROWSER_VM_FETCH_TIMEOUT_MS", "5");
    let codec = WireFrameCodec::default();
    let mut dispatcher = BrowserWireDispatcher::new(RecordingBridge::default());
    let mut config = KernelVmConfig::new("vm");
    config.permissions = Permissions::allow_all();
    dispatcher
        .sidecar_mut()
        .create_vm(config)
        .expect("create vm for vm.fetch");
    let context = dispatcher
        .sidecar_mut()
        .create_javascript_context(CreateJavascriptContextRequest {
            vm_id: String::from("vm"),
            bootstrap_module: None,
        })
        .expect("create context");
    let started = dispatcher
        .sidecar_mut()
        .start_execution(StartExecutionRequest {
            vm_id: String::from("vm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: String::from("/"),
        })
        .expect("start execution");
    dispatcher
        .sidecar_mut()
        .create_kernel_tcp_listener_for_execution(
            "vm",
            &started.execution_id,
            "127.0.0.1",
            3000,
            16,
        )
        .expect("create listener");

    let response = dispatch(
        &codec,
        &mut dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: String::from("conn"),
                session_id: String::from("session"),
                vm_id: String::from("vm"),
            }),
            payload: RequestPayload::VmFetchRequest(VmFetchRequest {
                port: 3000,
                method: String::from("GET"),
                path: String::from("/health"),
                headers_json: String::from("{}"),
                body: None,
                body_base64: None,
                stream_operation: None,
                stream_id: None,
                max_bytes: None,
            }),
        },
    );
    std::env::remove_var("AGENTOS_TEST_BROWSER_VM_FETCH_TIMEOUT_MS");

    let ResponsePayload::RejectedResponse(rejected) = response.payload else {
        panic!("unexpected vm.fetch response: {:?}", response.payload);
    };
    assert_eq!(rejected.code, "vm_fetch_failed");
    assert!(
        rejected
            .message
            .contains("timed out waiting for kernel TCP HTTP response"),
        "unexpected vm.fetch rejection: {}",
        rejected.message
    );
    assert!(
        !rejected.message.contains("not implemented"),
        "vm.fetch should no longer stop at the unsupported platform branch"
    );
}

fn test_bindings_payload(name: &str, alias: &str) -> RegisterHostCallbacksRequest {
    RegisterHostCallbacksRequest {
        name: name.to_string(),
        description: format!("{name} automation"),
        command_aliases: vec![alias.to_string()],
        registry_command_aliases: vec![String::from("agentos")],
        callbacks: std::collections::HashMap::from([(
            String::from("screenshot"),
            RegisteredHostCallbackDefinition {
                description: String::from("Take a screenshot"),
                input_schema: String::from(
                    r#"{"type":"object","properties":{},"additionalProperties":false}"#,
                ),
                timeout_ms: None,
                examples: Vec::new(),
            },
        )]),
    }
}

fn import_snapshot_layer(
    codec: &WireFrameCodec,
    dispatcher: &mut BrowserWireDispatcher<RecordingBridge>,
    ownership: OwnershipScope,
    request_id: i64,
    files: &[(&str, &str)],
) -> String {
    let response = dispatch(
        codec,
        dispatcher,
        RequestFrame {
            schema: protocol_schema(),
            request_id,
            ownership,
            payload: RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: files
                    .iter()
                    .map(|(path, content)| RootFilesystemEntry {
                        path: path.to_string(),
                        kind: RootFilesystemEntryKind::File,
                        mode: None,
                        uid: None,
                        gid: None,
                        content: Some(content.to_string()),
                        encoding: Some(RootFilesystemEntryEncoding::Utf8),
                        target: None,
                        executable: false,
                    })
                    .collect(),
            }),
        },
    );
    let ResponsePayload::SnapshotImportedResponse(imported) = response.payload else {
        panic!("unexpected import response: {:?}", response.payload);
    };
    imported.layer_id
}

fn dispatch(
    codec: &WireFrameCodec,
    dispatcher: &mut BrowserWireDispatcher<RecordingBridge>,
    request: RequestFrame,
) -> agentos_sidecar_protocol::wire::ResponseFrame {
    let request = codec
        .encode_message(&ProtocolFrame::RequestFrame(request))
        .expect("encode request");
    let response = dispatcher
        .handle_request_bytes(&request)
        .expect("dispatch request");
    let ProtocolFrame::ResponseFrame(response) =
        codec.decode_message(&response).expect("decode response")
    else {
        panic!("expected response frame");
    };
    response
}
