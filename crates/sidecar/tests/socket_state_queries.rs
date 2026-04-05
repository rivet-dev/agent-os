mod support;

use agent_os_sidecar::protocol::{
    DisposeReason, DisposeVmRequest, EventPayload, FindBoundUdpRequest, FindListenerRequest,
    GetSignalStateRequest, GuestRuntimeKind, OwnershipScope, RequestPayload, ResponsePayload,
    SignalDispositionAction,
};
use std::collections::BTreeMap;
use std::fs;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, create_vm_with_metadata, execute, new_sidecar,
    open_session, request, temp_dir, wasm_signal_state_module, write_fixture,
};

fn wait_for_process_output(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: &str,
) {
    let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll sidecar process output");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process output"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutput(output)
                if output.process_id == process_id && output.chunk.contains(expected) =>
            {
                return;
            }
            _ => {}
        }
    }
}

#[test]
fn sidecar_queries_listener_udp_and_signal_state() {
    assert_node_available();

    let mut sidecar = new_sidecar("socket-state-queries");
    let cwd = temp_dir("socket-state-queries-cwd");
    let tcp_entry = cwd.join("tcp-listener.mjs");
    let udp_entry = cwd.join("udp-listener.mjs");
    let signal_entry = cwd.join("signal-state.wasm");

    write_fixture(
        &tcp_entry,
        [
            "import net from 'node:net';",
            "const server = net.createServer(() => {});",
            "server.listen(43111, '0.0.0.0', () => {",
            "  console.log('tcp-listening:43111');",
            "});",
        ]
        .join("\n"),
    );
    write_fixture(
        &udp_entry,
        [
            "import dgram from 'node:dgram';",
            "const socket = dgram.createSocket('udp4');",
            "socket.bind(43112, '0.0.0.0', () => {",
            "  console.log('udp-bound:43112');",
            "});",
        ]
        .join("\n"),
    );
    fs::write(&signal_entry, wasm_signal_state_module()).expect("write signal-state wasm fixture");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let allowed_builtins = serde_json::to_string(&["net", "dgram"]).expect("serialize builtins");
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::from([(
            String::from("env.AGENT_OS_ALLOWED_NODE_BUILTINS"),
            allowed_builtins,
        )]),
    );
    let (wasm_vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        30,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Wasm,
        &cwd,
        BTreeMap::new(),
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "tcp-listener",
        GuestRuntimeKind::JavaScript,
        &tcp_entry,
        Vec::new(),
    );
    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "tcp-listener",
        "tcp-listening:43111",
    );

    execute(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "udp-listener",
        GuestRuntimeKind::JavaScript,
        &udp_entry,
        Vec::new(),
    );
    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "udp-listener",
        "udp-bound:43112",
    );

    execute(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &wasm_vm_id,
        "signal-state",
        GuestRuntimeKind::Wasm,
        &signal_entry,
        Vec::new(),
    );
    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &wasm_vm_id,
        "signal-state",
        "signal-registered",
    );

    let listener = sidecar
        .dispatch(request(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::FindListener(FindListenerRequest {
                host: Some(String::from("0.0.0.0")),
                port: Some(43111),
                path: None,
            }),
        ))
        .expect("query tcp listener");
    match listener.response.payload {
        ResponsePayload::ListenerSnapshot(snapshot) => {
            let listener = snapshot.listener.expect("listener snapshot");
            assert_eq!(listener.process_id, "tcp-listener");
            assert_eq!(listener.host.as_deref(), Some("0.0.0.0"));
            assert_eq!(listener.port, Some(43111));
        }
        other => panic!("unexpected listener response: {other:?}"),
    }

    let bound_udp = sidecar
        .dispatch(request(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::FindBoundUdp(FindBoundUdpRequest {
                host: Some(String::from("0.0.0.0")),
                port: Some(43112),
            }),
        ))
        .expect("query udp socket");
    match bound_udp.response.payload {
        ResponsePayload::BoundUdpSnapshot(snapshot) => {
            let socket = snapshot.socket.expect("bound udp snapshot");
            assert_eq!(socket.process_id, "udp-listener");
            assert_eq!(socket.host.as_deref(), Some("0.0.0.0"));
            assert_eq!(socket.port, Some(43112));
        }
        other => panic!("unexpected bound udp response: {other:?}"),
    }

    let signal_state = sidecar
        .dispatch(request(
            9,
            OwnershipScope::vm(&connection_id, &session_id, &wasm_vm_id),
            RequestPayload::GetSignalState(GetSignalStateRequest {
                process_id: String::from("signal-state"),
            }),
        ))
        .expect("query signal state");
    match signal_state.response.payload {
        ResponsePayload::SignalState(snapshot) => {
            assert_eq!(snapshot.process_id, "signal-state");
            assert_eq!(
                snapshot.handlers.get(&2),
                Some(&agent_os_sidecar::protocol::SignalHandlerRegistration {
                    action: SignalDispositionAction::User,
                    mask: vec![15],
                    flags: 0x1234,
                })
            );
        }
        other => panic!("unexpected signal state response: {other:?}"),
    }

    let dispose = sidecar
        .dispatch(request(
            10,
            OwnershipScope::vm(&connection_id, &session_id, &wasm_vm_id),
            RequestPayload::DisposeVm(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose wasm vm");
    match dispose.response.payload {
        ResponsePayload::VmDisposed(response) => {
            assert_eq!(response.vm_id, wasm_vm_id);
        }
        other => panic!("unexpected dispose response: {other:?}"),
    }
}
