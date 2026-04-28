mod support;

use agent_os_sidecar::protocol::{
    EventPayload, GetProcessSnapshotRequest, GetSignalStateRequest, GuestRuntimeKind,
    KillProcessRequest, OwnershipScope, ProcessSnapshotStatus, RequestPayload, ResponsePayload,
    SignalDispositionAction,
};
use nix::libc;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, create_vm_with_metadata, execute, new_sidecar,
    open_session, request, temp_dir, write_fixture,
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
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process output containing {expected:?}"
        );
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        let Some(event) = event else {
            continue;
        };
        if let EventPayload::ProcessOutput(output) = event.payload {
            if output.process_id == process_id && output.chunk.contains(expected) {
                return;
            }
        }
    }
}

fn wait_for_process_status(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    expected: ProcessSnapshotStatus,
) {
    let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let snapshot = sidecar
            .dispatch_blocking(request(
                0,
                ownership.clone(),
                RequestPayload::GetProcessSnapshot(GetProcessSnapshotRequest {}),
            ))
            .expect("query process snapshot");
        match snapshot.response.payload {
            ResponsePayload::ProcessSnapshot(snapshot) => {
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
            .poll_event_blocking(&ownership, Duration::from_millis(25))
            .expect("pump process events while waiting for status");
    }
}

fn embedded_runtime_signal_routes_sigterm_and_process_kill() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-signal-routing");
    let cwd = temp_dir("embedded-runtime-signal-routing-cwd");
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

    let connection_id = authenticate(&mut sidecar, "conn-embedded-runtime-signal-routing");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::new(),
    );

    execute(
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

    let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
    let registration_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let signal_state = sidecar
            .dispatch_blocking(request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalState(GetSignalStateRequest {
                    process_id: String::from("signal-routing"),
                }),
            ))
            .expect("query signal state");
        let ready = match signal_state.response.payload {
            ResponsePayload::SignalState(snapshot) => {
                snapshot.handlers.get(&(libc::SIGTERM as u32))
                    == Some(&agent_os_sidecar::protocol::SignalHandlerRegistration {
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
            .poll_event_blocking(&ownership, Duration::from_millis(25))
            .expect("pump signal registration events");
        assert!(
            Instant::now() < registration_deadline,
            "timed out waiting for SIGTERM registration"
        );
    }

    sidecar
        .dispatch_blocking(request(
            6,
            ownership.clone(),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("signal-routing"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("deliver SIGTERM");

    let event_deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_first_sigterm = false;
    let mut saw_second_sigterm = false;
    let mut exit_code = None;

    while exit_code.is_none() {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll signal events");
        let Some(event) = event else {
            assert!(
                Instant::now() < event_deadline,
                "timed out waiting for SIGTERM delivery"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutput(output) if output.process_id == "signal-routing" => {
                saw_first_sigterm |= output.chunk.contains("sigterm:1");
                saw_second_sigterm |= output.chunk.contains("sigterm:2");
            }
            EventPayload::ProcessExited(exited) if exited.process_id == "signal-routing" => {
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

fn embedded_runtime_signal_stop_continue_updates_kernel_state_and_guest_handler() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-signal-stop-cont");
    let cwd = temp_dir("embedded-runtime-signal-stop-cont-cwd");
    let entry = cwd.join("signal-stop-cont.mjs");

    write_fixture(
        &entry,
        [
            "let sigcontCount = 0;",
            "process.on('SIGCONT', () => {",
            "  sigcontCount += 1;",
            "  console.log(`sigcont:${sigcontCount}`);",
            "});",
            "console.log('ready');",
            "setInterval(() => {}, 25);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate(&mut sidecar, "conn-embedded-runtime-signal-stop-cont");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::new(),
    );

    execute(
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

    let ownership = OwnershipScope::vm(&connection_id, &session_id, &vm_id);
    sidecar
        .dispatch_blocking(request(
            5,
            ownership.clone(),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGSTOP"),
            }),
        ))
        .expect("deliver SIGSTOP");
    wait_for_process_status(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        ProcessSnapshotStatus::Stopped,
    );

    sidecar
        .dispatch_blocking(request(
            6,
            ownership.clone(),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGCONT"),
            }),
        ))
        .expect("deliver SIGCONT");
    wait_for_process_status(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        ProcessSnapshotStatus::Running,
    );
    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "signal-stop-cont",
        "sigcont:1",
    );

    sidecar
        .dispatch_blocking(request(
            7,
            ownership,
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("terminate stopped/continued process");
}

fn embedded_runtime_signal_delivers_sigchld_on_child_exit() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-signal-sigchld");
    let cwd = temp_dir("embedded-runtime-signal-sigchld-cwd");
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

    let connection_id = authenticate(&mut sidecar, "conn-embedded-runtime-signal-sigchld");
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
            .dispatch_blocking(request(
                5,
                ownership.clone(),
                RequestPayload::GetSignalState(GetSignalStateRequest {
                    process_id: String::from("sigchld-parent"),
                }),
            ))
            .expect("query SIGCHLD state");
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
            .poll_event_blocking(&ownership, Duration::from_millis(100))
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

#[test]
fn embedded_runtime_signal_suite() {
    embedded_runtime_signal_routes_sigterm_and_process_kill();
    embedded_runtime_signal_stop_continue_updates_kernel_state_and_guest_handler();
    embedded_runtime_signal_delivers_sigchld_on_child_exit();
}
