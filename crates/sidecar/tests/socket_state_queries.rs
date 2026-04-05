mod support;

use agent_os_sidecar::protocol::{
    DisposeReason, DisposeVmRequest, EventPayload, FindBoundUdpRequest, FindListenerRequest,
    GetSignalStateRequest, GuestRuntimeKind, KillProcessRequest, OwnershipScope, RequestPayload,
    ResponsePayload, SignalDispositionAction,
};
use nix::libc;
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
        GuestRuntimeKind::WebAssembly,
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

    let listener_deadline = Instant::now() + Duration::from_secs(5);
    loop {
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
                if let Some(listener) = snapshot.listener {
                    assert_eq!(listener.process_id, "tcp-listener");
                    assert_eq!(listener.host.as_deref(), Some("0.0.0.0"));
                    assert_eq!(listener.port, Some(43111));
                    break;
                }
            }
            other => panic!("unexpected listener response: {other:?}"),
        }
        assert!(
            Instant::now() < listener_deadline,
            "timed out waiting for listener snapshot"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let kill_listener = sidecar
        .dispatch(request(
            70,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("tcp-listener"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill tcp listener");
    assert!(matches!(
        kill_listener.response.payload,
        ResponsePayload::ProcessKilled(_)
    ));

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
        GuestRuntimeKind::WebAssembly,
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

    let signal_deadline = Instant::now() + Duration::from_secs(5);
    let wasm_ownership = OwnershipScope::vm(&connection_id, &session_id, &wasm_vm_id);
    loop {
        let _ = sidecar
            .poll_event(&wasm_ownership, Duration::from_millis(25))
            .expect("pump wasm signal-state events");
        let signal_state = sidecar
            .dispatch(request(
                9,
                wasm_ownership.clone(),
                RequestPayload::GetSignalState(GetSignalStateRequest {
                    process_id: String::from("signal-state"),
                }),
            ))
            .expect("query signal state");
        match signal_state.response.payload {
            ResponsePayload::SignalState(snapshot) => {
                assert_eq!(snapshot.process_id, "signal-state");
                if snapshot.handlers.get(&2)
                    == Some(&agent_os_sidecar::protocol::SignalHandlerRegistration {
                        action: SignalDispositionAction::User,
                        mask: vec![15],
                        flags: 0x1234,
                    })
                {
                    break;
                }
            }
            other => panic!("unexpected signal state response: {other:?}"),
        }
        assert!(
            Instant::now() < signal_deadline,
            "timed out waiting for signal state"
        );
        std::thread::sleep(Duration::from_millis(25));
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

#[test]
fn sidecar_tracks_javascript_sigchld_and_delivers_it_on_child_exit() {
    assert_node_available();

    let mut sidecar = new_sidecar("socket-state-sigchld");
    let cwd = temp_dir("socket-state-sigchld-cwd");
    let parent_entry = cwd.join("parent.mjs");
    let child_entry = cwd.join("child.mjs");

    write_fixture(
        &child_entry,
        [
            "await new Promise((resolve) => setTimeout(resolve, 200));",
            "console.log('child-exit');",
        ]
        .join("\n"),
    );
    write_fixture(
        &parent_entry,
        [
            "import { spawn } from 'node:child_process';",
            "let sigchldCount = 0;",
            "process.on('SIGCHLD', () => {",
            "  sigchldCount += 1;",
            "  console.log(`sigchld:${sigchldCount}`);",
            "});",
            "console.log('sigchld-registered');",
            "const child = spawn('node', ['./child.mjs'], { stdio: ['ignore', 'ignore', 'ignore'] });",
            "await new Promise((resolve, reject) => {",
            "  child.on('error', reject);",
            "  child.on('close', (code) => {",
            "    if (code !== 0) {",
            "      reject(new Error(`child exit ${code}`));",
            "      return;",
            "    }",
            "    resolve();",
            "  });",
            "});",
            "const deadline = Date.now() + 2000;",
            "while (sigchldCount === 0 && Date.now() < deadline) {",
            "  await new Promise((resolve) => setTimeout(resolve, 10));",
            "}",
            "if (sigchldCount === 0) {",
            "  throw new Error('SIGCHLD was not delivered');",
            "}",
            "console.log(`sigchld-final:${sigchldCount}`);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate(&mut sidecar, "conn-sigchld");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let allowed_builtins = serde_json::to_string(&[
        "assert",
        "buffer",
        "child_process",
        "console",
        "crypto",
        "events",
        "fs",
        "path",
        "querystring",
        "stream",
        "string_decoder",
        "timers",
        "url",
        "util",
        "zlib",
    ])
    .expect("serialize builtins");
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

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "sigchld-parent",
        GuestRuntimeKind::JavaScript,
        &parent_entry,
        Vec::new(),
    );

    let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut signal_registered = false;
    let mut saw_registered_output = false;
    let mut saw_sigchld_output = false;
    let mut saw_final_output = false;
    let mut exit_code = None;

    while exit_code.is_none() || !signal_registered {
        let signal_state = sidecar
            .dispatch(request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalState(GetSignalStateRequest {
                    process_id: String::from("sigchld-parent"),
                }),
            ))
            .expect("query sigchld signal state");
        match signal_state.response.payload {
            ResponsePayload::SignalState(snapshot) => {
                if snapshot.handlers.get(&(libc::SIGCHLD as u32))
                    == Some(&agent_os_sidecar::protocol::SignalHandlerRegistration {
                        action: SignalDispositionAction::User,
                        mask: vec![],
                        flags: 0,
                    })
                {
                    signal_registered = true;
                }
            }
            other => panic!("unexpected signal state response: {other:?}"),
        }

        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll SIGCHLD process");
        if let Some(event) = event {
            match event.payload {
                EventPayload::ProcessOutput(output) if output.process_id == "sigchld-parent" => {
                    saw_registered_output |= output.chunk.contains("sigchld-registered");
                    saw_sigchld_output |= output.chunk.contains("sigchld:1");
                    saw_final_output |= output.chunk.contains("sigchld-final:1");
                }
                EventPayload::ProcessExited(exited) if exited.process_id == "sigchld-parent" => {
                    exit_code = Some(exited.exit_code);
                }
                _ => {}
            }
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for SIGCHLD registration/output"
        );
    }

    assert!(signal_registered, "SIGCHLD should be registered");
    assert!(
        saw_registered_output,
        "parent should report SIGCHLD registration"
    );
    assert!(saw_sigchld_output, "parent should receive SIGCHLD output");
    assert!(saw_final_output, "parent should report final SIGCHLD count");
    assert_eq!(exit_code, Some(0));
}
