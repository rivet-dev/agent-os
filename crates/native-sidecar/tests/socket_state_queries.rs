mod support;

use agentos_native_sidecar::wire::{
    EventPayload, FindBoundUdpRequest, FindListenerRequest, GetSignalStateRequest,
    GuestRuntimeKind, KillProcessRequest, ProcessSnapshotStatus, RequestPayload, ResponsePayload,
    SignalDispositionAction, SignalHandlerRegistration,
};
use nix::libc;
use std::collections::HashMap;
use std::fs;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire_with_metadata, execute_wire,
    new_sidecar, open_session_wire, temp_dir, wasm_signal_state_module, wire_request, wire_vm,
    write_fixture, write_guest_file_wire,
};

fn wait_for_process_output(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: &str,
) {
    let ownership = wire_vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process output"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar process output");
        let Some(event) = event else {
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == process_id
                    && String::from_utf8_lossy(&output.chunk).contains(expected) =>
            {
                return;
            }
            _ => {}
        }
    }
}

fn wait_for_process_status(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: ProcessSnapshotStatus,
) {
    let ownership = wire_vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let snapshot = sidecar
            .dispatch_wire_blocking(wire_request(
                1,
                ownership.clone(),
                RequestPayload::GetProcessSnapshotRequest,
            ))
            .expect("query process snapshot");
        match snapshot.response.payload {
            ResponsePayload::ProcessSnapshotResponse(snapshot) => {
                if snapshot
                    .processes
                    .iter()
                    .find(|entry| entry.process_id == process_id)
                    .is_some_and(|entry| entry.status == expected)
                {
                    return;
                }
            }
            other => panic!("unexpected process snapshot response: {other:?}"),
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for process status {expected:?}"
        );
        let _ = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(25))
            .expect("pump process events while waiting for status");
    }
}

fn v8_signal_delivery_routes_kill_process_and_process_kill() {
    assert_node_available();

    let mut sidecar = new_sidecar("v8-signal-routing");
    let cwd = temp_dir("v8-signal-routing-cwd");
    let entry = cwd.join("signal-routing.mjs");

    write_fixture(
        &entry,
        [
            "let sigtermCount = 0;",
            "process.on('SIGHUP', () => {});",
            "process.on('SIGWINCH', () => {});",
            "process.on('SIGTERM', () => {",
            "  sigtermCount += 1;",
            "  console.log(`sigterm:${sigtermCount}`);",
            "  if (sigtermCount === 1) {",
            "    process.kill(process.pid, 'SIGTERM');",
            "    return;",
            "  }",
            "  process.exit(0);",
            "});",
            "console.log('signal-handlers-ready');",
            "setInterval(() => {}, 25);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-v8-signal-routing");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        HashMap::new(),
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-routing",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-routing",
        "signal-handlers-ready",
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let signal_state = sidecar
            .dispatch_wire_blocking(wire_request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                    process_id: String::from("signal-routing"),
                }),
            ))
            .expect("query V8 signal state");
        let ready = match signal_state.response.payload {
            ResponsePayload::SignalStateResponse(snapshot) => {
                snapshot.handlers.get(&(libc::SIGTERM as u32))
                    == Some(&SignalHandlerRegistration {
                        action: SignalDispositionAction::User,
                        mask: vec![],
                        flags: 0,
                    })
                    && snapshot.handlers.get(&(libc::SIGHUP as u32))
                        == Some(&SignalHandlerRegistration {
                            action: SignalDispositionAction::User,
                            mask: vec![],
                            flags: 0,
                        })
                    && snapshot.handlers.get(&(libc::SIGWINCH as u32))
                        == Some(&SignalHandlerRegistration {
                            action: SignalDispositionAction::User,
                            mask: vec![],
                            flags: 0,
                        })
            }
            other => panic!("unexpected signal state response: {other:?}"),
        };
        if ready {
            break;
        }
        let _ = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(25))
            .expect("pump V8 signal registration events");
        assert!(
            Instant::now() < registration_deadline,
            "timed out waiting for V8 signal registrations"
        );
    }

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("signal-routing"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("deliver SIGTERM to V8 guest");

    let event_deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_first_sigterm = false;
    let mut saw_second_sigterm = false;
    let mut exit_code = None;

    while exit_code.is_none() {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll V8 signal events");
        let Some(event) = event else {
            assert!(
                Instant::now() < event_deadline,
                "timed out waiting for V8 signal delivery"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutputEvent(output) if output.process_id == "signal-routing" => {
                let chunk = String::from_utf8_lossy(&output.chunk);
                saw_first_sigterm |= chunk.contains("sigterm:1");
                saw_second_sigterm |= chunk.contains("sigterm:2");
            }
            EventPayload::ProcessExitedEvent(exited) if exited.process_id == "signal-routing" => {
                exit_code = Some(exited.exit_code);
            }
            _ => {}
        }
    }

    assert!(saw_first_sigterm, "expected control-plane SIGTERM delivery");
    assert!(
        saw_second_sigterm,
        "expected guest process.kill(SIGTERM) delivery"
    );
    assert_eq!(exit_code, Some(0));
}

fn v8_signal_stop_and_continue_updates_process_snapshot() {
    assert_node_available();

    let mut sidecar = new_sidecar("v8-signal-stop-cont");
    let cwd = temp_dir("v8-signal-stop-cont-cwd");
    let entry = cwd.join("signal-stop-cont.mjs");

    write_fixture(
        &entry,
        ["console.log('ready');", "setInterval(() => {}, 25);"].join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-v8-signal-stop-cont");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        HashMap::new(),
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        "ready",
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGSTOP"),
            }),
        ))
        .expect("deliver SIGSTOP to V8 guest");
    wait_for_process_status(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        ProcessSnapshotStatus::Stopped,
    );

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGCONT"),
            }),
        ))
        .expect("deliver SIGCONT to V8 guest");
    wait_for_process_status(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        ProcessSnapshotStatus::Running,
    );

    sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            ownership,
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("terminate V8 guest after stop/cont");
}

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
            "server.listen(43111, '127.0.0.1', () => {",
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
            "socket.bind(43112, '127.0.0.1', () => {",
            "  console.log('udp-bound:43112');",
            "});",
        ]
        .join("\n"),
    );
    fs::write(&signal_entry, wasm_signal_state_module()).expect("write signal-state wasm fixture");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let allowed_builtins = serde_json::to_string(&["net", "dgram"]).expect("serialize builtins");
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_ALLOWED_NODE_BUILTINS"),
            allowed_builtins,
        )]),
    );
    let (wasm_vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        30,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::new(),
    );
    let (other_vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        31,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        HashMap::new(),
    );

    execute_wire(
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
            .dispatch_wire_blocking(wire_request(
                7,
                wire_vm(&connection_id, &session_id, &vm_id),
                RequestPayload::FindListenerRequest(FindListenerRequest {
                    host: Some(String::from("127.0.0.1")),
                    port: Some(43111),
                    path: None,
                }),
            ))
            .expect("query tcp listener");
        match listener.response.payload {
            ResponsePayload::ListenerSnapshotResponse(snapshot) => {
                if let Some(listener) = snapshot.listener {
                    assert_eq!(listener.process_id, "tcp-listener");
                    assert_eq!(listener.host.as_deref(), Some("127.0.0.1"));
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

    let other_vm_listener = sidecar
        .dispatch_wire_blocking(wire_request(
            71,
            wire_vm(&connection_id, &session_id, &other_vm_id),
            RequestPayload::FindListenerRequest(FindListenerRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(43111),
                path: None,
            }),
        ))
        .expect("query tcp listener from another vm");
    match other_vm_listener.response.payload {
        ResponsePayload::ListenerSnapshotResponse(snapshot) => {
            assert!(
                snapshot.listener.is_none(),
                "listener from vm {vm_id} leaked into vm {other_vm_id}: {:?}",
                snapshot.listener
            );
        }
        other => panic!("unexpected other-vm listener response: {other:?}"),
    }

    let kill_listener = sidecar
        .dispatch_wire_blocking(wire_request(
            70,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("tcp-listener"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill tcp listener");
    assert!(matches!(
        kill_listener.response.payload,
        ResponsePayload::ProcessKilledResponse(_)
    ));

    execute_wire(
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

    execute_wire(
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
    let wasm_ownership = wire_vm(&connection_id, &session_id, &wasm_vm_id);

    let bound_udp = sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::FindBoundUdpRequest(FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(43112),
            }),
        ))
        .expect("query udp socket");
    match bound_udp.response.payload {
        ResponsePayload::BoundUdpSnapshotResponse(snapshot) => {
            let socket = snapshot.socket.expect("bound udp snapshot");
            assert_eq!(socket.process_id, "udp-listener");
            assert_eq!(socket.host.as_deref(), Some("127.0.0.1"));
            assert_eq!(socket.port, Some(43112));
        }
        other => panic!("unexpected bound udp response: {other:?}"),
    }

    let other_vm_bound_udp = sidecar
        .dispatch_wire_blocking(wire_request(
            72,
            wire_vm(&connection_id, &session_id, &other_vm_id),
            RequestPayload::FindBoundUdpRequest(FindBoundUdpRequest {
                host: Some(String::from("127.0.0.1")),
                port: Some(43112),
            }),
        ))
        .expect("query udp socket from another vm");
    match other_vm_bound_udp.response.payload {
        ResponsePayload::BoundUdpSnapshotResponse(snapshot) => {
            assert!(
                snapshot.socket.is_none(),
                "udp socket from vm {vm_id} leaked into vm {other_vm_id}: {:?}",
                snapshot.socket
            );
        }
        other => panic!("unexpected other-vm udp response: {other:?}"),
    }

    let signal_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = sidecar
            .poll_event_wire_blocking(&wasm_ownership, Duration::from_millis(25))
            .expect("pump wasm signal-state events");
        let signal_state = sidecar
            .dispatch_wire_blocking(wire_request(
                9,
                wasm_ownership.clone(),
                RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                    process_id: String::from("signal-state"),
                }),
            ))
            .expect("query signal state");
        match signal_state.response.payload {
            ResponsePayload::SignalStateResponse(snapshot) => {
                assert_eq!(snapshot.process_id, "signal-state");
                if snapshot.handlers.get(&2)
                    == Some(&SignalHandlerRegistration {
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
}

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

    let connection_id = authenticate_wire(&mut sidecar, "conn-sigchld");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
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
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_ALLOWED_NODE_BUILTINS"),
            allowed_builtins,
        )]),
    );
    write_guest_file_wire(
        &mut sidecar,
        3_001,
        &connection_id,
        &session_id,
        &vm_id,
        "/child.mjs",
        fs::read_to_string(&child_entry).expect("read SIGCHLD child fixture"),
    );

    execute_wire(
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

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut signal_registered = false;
    let mut saw_registered_output = false;
    let mut saw_sigchld_output = false;
    let mut saw_final_output = false;
    let mut exit_code = None;

    while exit_code.is_none() || !signal_registered {
        let signal_state = sidecar
            .dispatch_wire_blocking(wire_request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalStateRequest(GetSignalStateRequest {
                    process_id: String::from("sigchld-parent"),
                }),
            ))
            .expect("query sigchld signal state");
        match signal_state.response.payload {
            ResponsePayload::SignalStateResponse(snapshot) => {
                if snapshot.handlers.get(&(libc::SIGCHLD as u32))
                    == Some(&SignalHandlerRegistration {
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
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll SIGCHLD process");
        if let Some(event) = event {
            match event.payload {
                EventPayload::ProcessOutputEvent(output)
                    if output.process_id == "sigchld-parent" =>
                {
                    let chunk = String::from_utf8_lossy(&output.chunk);
                    saw_registered_output |= chunk.contains("sigchld-registered");
                    saw_sigchld_output |= chunk.contains("sigchld:1");
                    saw_final_output |= chunk.contains("sigchld-final:1");
                }
                EventPayload::ProcessExitedEvent(exited)
                    if exited.process_id == "sigchld-parent" =>
                {
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

#[test]
fn socket_state_queries_suite() {
    // Multiple libtest cases in this V8-backed integration binary still trip
    // teardown/init crashes, so keep the coverage in one top-level suite.
    v8_signal_delivery_routes_kill_process_and_process_kill();
    v8_signal_stop_and_continue_updates_process_snapshot();
    sidecar_queries_listener_udp_and_signal_state();
    sidecar_tracks_javascript_sigchld_and_delivers_it_on_child_exit();
}
