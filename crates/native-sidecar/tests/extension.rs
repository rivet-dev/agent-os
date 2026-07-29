mod support;

use std::collections::HashMap;
use std::fs;
use std::time::Duration;

use agentos_bridge::{LoadFilesystemStateRequest, PersistenceBridge};
use agentos_native_sidecar::wire::{
    EventPayload, ExecuteRequest, ExtEnvelope, GuestFilesystemCallRequest,
    GuestFilesystemOperation, GuestRuntimeKind, RequestPayload, ResponsePayload,
    SidecarRequestPayload, SidecarResponseFrame, SidecarResponsePayload, StreamChannel,
    VmLifecycleState,
};
use agentos_native_sidecar::{
    Extension, ExtensionContext, ExtensionFuture, ExtensionResponse, SidecarError,
};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, new_sidecar, open_session_wire,
    temp_dir, wire_request, wire_vm, RecordingBridge,
};

const TEST_NAMESPACE: &str = "dev.rivet.agentos.extension-test";

struct EchoExtension;
struct VmLifetimeExtension;

impl Extension for EchoExtension {
    fn namespace(&self) -> &str {
        TEST_NAMESPACE
    }

    fn handle_request<'a>(
        &'a self,
        mut ctx: ExtensionContext<'a>,
        payload: Vec<u8>,
    ) -> ExtensionFuture<'a, ExtensionResponse> {
        Box::pin(async move {
            let callback =
                ctx.invoke_callback(b"callback-input".to_vec(), Duration::from_secs(1))?;
            let payload = String::from_utf8(payload).map_err(|error| {
                SidecarError::InvalidState(format!("invalid extension test entrypoint: {error}"))
            })?;
            let mut payload_lines = payload.lines();
            let entrypoint = payload_lines
                .next()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from("missing extension process entrypoint"))
                })?
                .to_string();
            let lifecycle_entrypoint = payload_lines
                .next()
                .ok_or_else(|| {
                    SidecarError::InvalidState(String::from(
                        "missing extension lifecycle entrypoint",
                    ))
                })?
                .to_string();
            let process_id = "extension-process";
            ctx.start_buffering_process_output(process_id).await?;
            ctx.guest_filesystem_call_wire(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/tmp/extension-fs.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("extension fs primitive")),
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
            })
            .await?;
            let fs_read = ctx
                .guest_filesystem_call_wire(GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::ReadFile,
                    path: String::from("/tmp/extension-fs.txt"),
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
                })
                .await?;
            assert_eq!(fs_read.content.as_deref(), Some("extension fs primitive"));

            let started = ctx
                .spawn_process_wire(ExecuteRequest {
                    process_id: process_id.to_string(),
                    command: None,
                    runtime: Some(GuestRuntimeKind::JavaScript),
                    entrypoint: Some(entrypoint),
                    args: Vec::new(),
                    env: HashMap::new(),
                    cwd: None,
                    wasm_permission_tier: None,
                })
                .await?;
            assert_eq!(started.process_id, process_id);
            let handoff = ctx
                .handoff_buffered_process_output(
                    "extension-buffered-session",
                    process_id,
                    Duration::from_secs(5),
                )
                .await?;
            assert!(String::from_utf8_lossy(&handoff.stdout).contains("extension-buffered-output"));
            assert!(!handoff.stdout_truncated);
            let lifecycle_process_id = "extension-lifecycle-process";
            let lifecycle_started = ctx
                .spawn_process_wire(ExecuteRequest {
                    process_id: lifecycle_process_id.to_string(),
                    command: None,
                    runtime: Some(GuestRuntimeKind::JavaScript),
                    entrypoint: Some(lifecycle_entrypoint),
                    args: Vec::new(),
                    env: HashMap::new(),
                    cwd: None,
                    wasm_permission_tier: None,
                })
                .await?;
            assert_eq!(lifecycle_started.process_id, lifecycle_process_id);
            ctx.bind_process_to_session("extension-lifecycle-session", lifecycle_process_id)
                .await?;
            ctx.dispose_session_resources("extension-lifecycle-session")
                .await?;

            let mut stdout = handoff.stdout;
            let mut exit_code = None;
            let mut lifecycle_exit_code = None;
            while exit_code.is_none() || lifecycle_exit_code.is_none() {
                let event = ctx
                    .poll_event_wire(Duration::from_secs(5))
                    .await?
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "timed out waiting for extension process event",
                        ))
                    })?;
                match event.payload {
                    EventPayload::ProcessOutputEvent(output)
                        if output.process_id == process_id
                            && output.channel == StreamChannel::Stdout =>
                    {
                        stdout.extend(output.chunk);
                    }
                    EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                        exit_code = Some(exited.exit_code);
                    }
                    EventPayload::ProcessExitedEvent(exited)
                        if exited.process_id == lifecycle_process_id =>
                    {
                        lifecycle_exit_code = Some(exited.exit_code);
                    }
                    EventPayload::ProcessOutputEvent(_)
                    | EventPayload::ProcessExitedEvent(_)
                    | EventPayload::VmLifecycleEvent(_)
                    | EventPayload::StructuredEvent(_)
                    | EventPayload::ExtEnvelope(_) => {}
                }
            }

            let stdout = String::from_utf8(stdout).map_err(|error| {
                SidecarError::InvalidState(format!("invalid extension process stdout: {error}"))
            })?;
            let process_summary = format!(
                "{}:{}:{}",
                String::from_utf8_lossy(&callback),
                stdout.trim().replace('\n', "|"),
                exit_code.expect("exit code set before loop exits"),
            );
            ExtensionResponse::with_wire_events(
                process_summary.clone().into_bytes(),
                vec![ctx.ext_event_wire(format!("extension-event:{process_summary}").into_bytes())?],
            )
        })
    }
}

impl Extension for VmLifetimeExtension {
    fn namespace(&self) -> &str {
        "dev.rivet.agentos.extension-vm-lifetime-test"
    }

    fn handle_request<'a>(
        &'a self,
        mut ctx: ExtensionContext<'a>,
        _payload: Vec<u8>,
    ) -> ExtensionFuture<'a, ExtensionResponse> {
        Box::pin(async move {
            ctx.bind_vm_to_session("extension-vm-session").await?;
            let events = ctx
                .dispose_session_resources_wire("extension-vm-session")
                .await?;
            ExtensionResponse::with_wire_events(b"vm-disposed".to_vec(), events)
        })
    }
}

#[test]
fn registered_extension_round_trips_ext_request_callback_and_event() {
    assert_node_available();
    let mut sidecar = new_sidecar("extension-roundtrip");
    sidecar
        .register_extension(Box::new(EchoExtension))
        .expect("register extension");
    sidecar.set_wire_sidecar_request_handler(|frame| match frame.payload {
        SidecarRequestPayload::ExtEnvelope(envelope) => {
            assert_eq!(envelope.namespace, TEST_NAMESPACE);
            assert_eq!(envelope.payload, b"callback-input");
            Ok(SidecarResponseFrame {
                schema: frame.schema,
                request_id: frame.request_id,
                ownership: frame.ownership,
                payload: SidecarResponsePayload::ExtEnvelope(ExtEnvelope {
                    namespace: envelope.namespace,
                    payload: b"callback-output".to_vec(),
                }),
            })
        }
        other => panic!("unexpected sidecar request payload: {other:?}"),
    });

    let connection_id = authenticate_wire(&mut sidecar, "extension-client");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("extension-process-cwd");
    let entrypoint = cwd.join("extension-entrypoint.mjs");
    let lifecycle_entrypoint = cwd.join("extension-lifecycle-entrypoint.mjs");
    fs::write(
        &entrypoint,
        "console.log('extension-buffered-output');\nsetTimeout(() => {\n  console.log('extension-process-output');\n  process.exit(0);\n}, 50);\n",
    )
    .expect("write extension entrypoint");
    fs::write(&lifecycle_entrypoint, "setInterval(() => {}, 1000);\n")
        .expect("write extension lifecycle entrypoint");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: TEST_NAMESPACE.to_string(),
                payload: format!(
                    "{}\n{}",
                    entrypoint.to_string_lossy(),
                    lifecycle_entrypoint.to_string_lossy()
                )
                .into_bytes(),
            }),
        ))
        .expect("dispatch extension request");

    match result.response.payload {
        ResponsePayload::ExtEnvelope(envelope) => {
            assert_eq!(envelope.namespace, TEST_NAMESPACE);
            assert_eq!(
                envelope.payload,
                b"callback-output:extension-buffered-output|extension-process-output:0"
            );
        }
        other => panic!("unexpected extension response: {other:?}"),
    }

    assert_eq!(result.events.len(), 1);
    match &result.events[0].payload {
        EventPayload::ExtEnvelope(envelope) => {
            assert_eq!(envelope.namespace, TEST_NAMESPACE);
            assert_eq!(
                envelope.payload,
                b"extension-event:callback-output:extension-buffered-output|extension-process-output:0",
            );
        }
        other => panic!("unexpected extension event: {other:?}"),
    }
}

#[test]
fn extension_session_resources_can_dispose_bound_vm() {
    assert_node_available();
    let mut sidecar = new_sidecar("extension-vm-lifetime");
    sidecar
        .register_extension(Box::new(VmLifetimeExtension))
        .expect("register vm lifetime extension");

    let connection_id = authenticate_wire(&mut sidecar, "extension-vm-client");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("extension-vm-lifetime-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from("dev.rivet.agentos.extension-vm-lifetime-test"),
                payload: Vec::new(),
            }),
        ))
        .expect("dispatch vm lifetime extension request");

    match result.response.payload {
        ResponsePayload::ExtEnvelope(envelope) => {
            assert_eq!(
                envelope.namespace,
                "dev.rivet.agentos.extension-vm-lifetime-test"
            );
            assert_eq!(envelope.payload, b"vm-disposed");
        }
        other => panic!("unexpected extension response: {other:?}"),
    }
    assert!(result.events.iter().any(|event| {
        matches!(&event.payload, EventPayload::VmLifecycleEvent(event) if event.state == VmLifecycleState::Disposed)
    }));

    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Exists,
                path: String::from("/tmp/extension-fs.txt"),
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
        ))
        .expect("dispatch call against disposed vm");
    match rejected.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains(&vm_id));
        }
        other => panic!("unexpected disposed-vm response: {other:?}"),
    }

    sidecar
        .with_bridge_mut(|bridge: &mut RecordingBridge| {
            let snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: vm_id.clone(),
                })
                .expect("load persisted snapshot");
            assert!(
                snapshot.is_some(),
                "extension-bound vm disposal should flush a filesystem snapshot"
            );
        })
        .expect("inspect persistence bridge");
}

#[test]
fn duplicate_extension_namespaces_are_rejected() {
    let mut sidecar = new_sidecar("extension-duplicate");
    sidecar
        .register_extension(Box::new(EchoExtension))
        .expect("register first extension");

    let error = sidecar
        .register_extension(Box::new(EchoExtension))
        .expect_err("duplicate extension namespace should fail");
    assert!(matches!(error, SidecarError::Conflict(_)));
}
