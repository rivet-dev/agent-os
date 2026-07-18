mod support;

use agentos_native_sidecar::wire::{
    EventPayload, GetSignalStateRequest, GuestRuntimeKind, KillProcessRequest,
    ProcessSnapshotStatus, RequestPayload, ResizePtyRequest, ResponsePayload,
    SignalDispositionAction, SignalHandlerRegistration, StreamChannel,
};
use nix::libc;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_wire_with_metadata, execute_wire, new_sidecar, open_session_wire, temp_dir,
    wire_request, wire_vm, write_fixture, write_guest_file_wire,
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
            "timed out waiting for process output containing {expected:?}"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        let Some(event) = event else {
            continue;
        };
        if let EventPayload::ProcessOutputEvent(output) = event.payload {
            if output.process_id == process_id
                && String::from_utf8_lossy(&output.chunk).contains(expected)
            {
                return;
            }
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-signal-routing");
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
            .expect("query signal state");
        let ready = match signal_state.response.payload {
            ResponsePayload::SignalStateResponse(snapshot) => {
                snapshot.handlers.get(&(libc::SIGTERM as u32))
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
            .expect("pump signal registration events");
        assert!(
            Instant::now() < registration_deadline,
            "timed out waiting for SIGTERM registration"
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
        .expect("deliver SIGTERM");

    let event_deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_first_sigterm = false;
    let mut saw_second_sigterm = false;
    let mut exit_code = None;

    while exit_code.is_none() {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll signal events");
        let Some(event) = event else {
            assert!(
                Instant::now() < event_deadline,
                "timed out waiting for SIGTERM delivery"
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-signal-stop-cont");
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
        .dispatch_wire_blocking(wire_request(
            6,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
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
        .dispatch_wire_blocking(wire_request(
            7,
            ownership,
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("signal-stop-cont"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("terminate stopped/continued process");
}

fn embedded_runtime_kill_process_rejects_invalid_signal_without_killing_process() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-invalid-signal");
    let cwd = temp_dir("embedded-runtime-invalid-signal-cwd");
    let entry = cwd.join("invalid-signal.mjs");

    write_fixture(
        &entry,
        [
            "console.log('invalid-signal-ready');",
            "setInterval(() => {}, 25);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-invalid-signal");
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
        "invalid-signal",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "invalid-signal",
        "invalid-signal-ready",
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let invalid_signal = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            ownership.clone(),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("invalid-signal"),
                signal: String::from("SIGBOGUS"),
            }),
        ))
        .expect("dispatch invalid signal");
    let ResponsePayload::RejectedResponse(response) = invalid_signal.response.payload else {
        panic!("unexpected invalid signal response");
    };
    assert_eq!(response.code, "invalid_state");
    assert!(
        response.message.contains("unsupported kill_process signal"),
        "unexpected invalid signal rejection: {}",
        response.message
    );

    wait_for_process_status(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "invalid-signal",
        ProcessSnapshotStatus::Running,
    );

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            ownership,
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("invalid-signal"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("terminate invalid-signal process");
}

fn embedded_runtime_process_kill_signal_zero_checks_child_liveness() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-process-kill-sig0");
    let cwd = temp_dir("embedded-runtime-process-kill-sig0-cwd");
    let entry = cwd.join("process-kill-sig0.mjs");

    write_fixture(
        &entry,
        [
            "const { spawn, spawnSync } = require('node:child_process');",
            "const live = spawn(process.execPath, ['-e', 'setTimeout(() => {}, 5000)'], { stdio: 'ignore' });",
            "console.log(`live:${process.kill(live.pid, 0)}`);",
            "live.kill('SIGTERM');",
            "const stale = spawnSync(process.execPath, ['-e', ''], { encoding: 'utf8' });",
            "if (typeof stale.pid !== 'number') {",
            "  throw new Error('spawnSync result did not include child pid');",
            "}",
            "let staleResult = 'alive';",
            "try {",
            "  process.kill(stale.pid, 0);",
            "} catch (error) {",
            "  staleResult = error && typeof error.code === 'string' ? error.code : 'error';",
            "}",
            "console.log(`stale:${staleResult}`);",
            "process.exit(staleResult === 'alive' ? 1 : 0);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-process-kill-sig0");
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
        "process-kill-sig0",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_live = false;
    let mut saw_stale_esrch = false;
    let mut exit_code = None;

    while exit_code.is_none() || !saw_live || !saw_stale_esrch {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll process.kill signal-zero events");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process.kill signal-zero output"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == "process-kill-sig0" =>
            {
                let chunk = String::from_utf8_lossy(&output.chunk);
                saw_live |= chunk.contains("live:true");
                saw_stale_esrch |= chunk.contains("stale:ESRCH");
            }
            EventPayload::ProcessExitedEvent(exited)
                if exited.process_id == "process-kill-sig0" =>
            {
                exit_code = Some(exited.exit_code);
            }
            _ => {}
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for process.kill signal-zero completion"
        );
    }

    assert!(saw_live, "live child should be visible to signal 0");
    assert!(
        saw_stale_esrch,
        "stale child PID should throw ESRCH for signal 0"
    );
    assert_eq!(exit_code, Some(0));
}

fn embedded_runtime_process_group_kill_terminates_detached_tree() {
    assert_node_available();

    let mut sidecar = new_sidecar("embedded-runtime-process-group-kill");
    let cwd = temp_dir("embedded-runtime-process-group-kill-cwd");
    let parent_entry = cwd.join("group-parent.mjs");
    let child_entry = cwd.join("group-child.mjs");

    write_fixture(
        &child_entry,
        [
            "import { spawn } from 'node:child_process';",
            "const makeChild = () => spawn(",
            "  process.execPath,",
            "  ['-e', 'setTimeout(() => {}, 100000)'],",
            "  { stdio: ['ignore', 'ignore', 'ignore'] },",
            ");",
            "const first = makeChild();",
            "const second = makeChild();",
            "console.log(`group-ready:${first.pid}:${second.pid}`);",
            "setInterval(() => {}, 1000);",
        ]
        .join("\n"),
    );
    write_fixture(
        &parent_entry,
        [
            "import { spawn } from 'node:child_process';",
            "const child = spawn(process.execPath, ['./group-child.mjs'], {",
            "  detached: true,",
            "  stdio: ['ignore', 'pipe', 'pipe'],",
            "});",
            "let buffered = '';",
            "const grandchildPids = await new Promise((resolve, reject) => {",
            "  child.on('error', reject);",
            "  child.stdout.on('data', (chunk) => {",
            "    buffered += chunk.toString();",
            "    const match = buffered.match(/group-ready:(\\d+):(\\d+)/);",
            "    if (match) {",
            "      resolve([Number(match[1]), Number(match[2])]);",
            "    }",
            "  });",
            "});",
            "const closePromise = new Promise((resolve) => {",
            "  child.on('close', (code, signal) => resolve({ code, signal }));",
            "});",
            "const killResult = process.kill(-child.pid, 'SIGKILL');",
            "console.log('kill-returned:' + killResult);",
            "const closed = await closePromise;",
            "console.log('group-close:' + closed.code + ':' + closed.signal);",
            "const errorCode = (error) => {",
            "  if (error && typeof error.code === 'string' && error.syscall === 'kill') {",
            "    return error.code;",
            "  }",
            "  return 'missing-errno-error';",
            "};",
            "const probe = (pid) => {",
            "  try {",
            "    process.kill(pid, 0);",
            "    return 'alive';",
            "  } catch (error) {",
            "    return errorCode(error);",
            "  }",
            "};",
            "console.log('probe-child:' + probe(child.pid));",
            "console.log('probe-grandchild-a:' + probe(grandchildPids[0]));",
            "console.log('probe-grandchild-b:' + probe(grandchildPids[1]));",
            "let missingGroup;",
            "try {",
            "  process.kill(-999999, 'SIGKILL');",
            "  missingGroup = 'no-error';",
            "} catch (error) {",
            "  missingGroup = errorCode(error);",
            "}",
            "console.log('probe-missing-group:' + missingGroup);",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-process-group-kill");
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
    write_guest_file_wire(
        &mut sidecar,
        3_001,
        &connection_id,
        &session_id,
        &vm_id,
        "/group-child.mjs",
        std::fs::read_to_string(&child_entry).expect("read process-group child fixture"),
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "group-kill-parent",
        GuestRuntimeKind::JavaScript,
        &parent_entry,
        Vec::new(),
    );

    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit_code = None;

    while exit_code.is_none() {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll process group kill events");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for group kill completion\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == "group-kill-parent" =>
            {
                let chunk = String::from_utf8_lossy(&output.chunk);
                match output.channel {
                    StreamChannel::Stdout => stdout.push_str(&chunk),
                    StreamChannel::Stderr => stderr.push_str(&chunk),
                }
            }
            EventPayload::ProcessExitedEvent(exited)
                if exited.process_id == "group-kill-parent" =>
            {
                exit_code = Some(exited.exit_code);
            }
            _ => {}
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for group kill completion\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }

    assert_eq!(
        exit_code,
        Some(0),
        "group kill parent should exit cleanly\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("kill-returned:true"),
        "group kill should report success\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("group-close:"),
        "detached child should emit close after group kill\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("probe-child:ESRCH"),
        "killed group leader should probe as ESRCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("probe-grandchild-a:ESRCH"),
        "first grandchild should be killed with the group\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("probe-grandchild-b:ESRCH"),
        "second grandchild should be killed with the group\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("probe-missing-group:ESRCH"),
        "missing process group should raise ESRCH\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

fn pty_resize_delivers_sigwinch_to_nested_foreground_runtime() {
    assert_node_available();

    let mut sidecar = new_sidecar("pty-resize-nested-sigwinch");
    let cwd = temp_dir("pty-resize-nested-sigwinch-cwd");
    let parent_entry = cwd.join("parent.mjs");
    let child_entry = cwd.join("child.mjs");
    write_fixture(
        &child_entry,
        [
            "process.on('SIGWINCH', () => {",
            "  console.log('child-winch');",
            "  process.exit(0);",
            "});",
            "console.log('child-winch-ready');",
            "setInterval(() => {}, 25);",
        ]
        .join("\n"),
    );
    write_fixture(
        &parent_entry,
        [
            "import { spawn } from 'node:child_process';",
            "let parentWinch = false;",
            "process.on('SIGWINCH', () => {",
            "  parentWinch = true;",
            "  console.log('parent-winch');",
            "});",
            "const child = spawn('node', ['./child.mjs'], { stdio: 'inherit' });",
            "await new Promise((resolve, reject) => {",
            "  child.on('error', reject);",
            "  child.on('close', (code) => code === 0 ? resolve() : reject(new Error(`child exit ${code}`)));",
            "});",
            "const deadline = Date.now() + 2000;",
            "while (!parentWinch && Date.now() < deadline) {",
            "  await new Promise((resolve) => setTimeout(resolve, 10));",
            "}",
            "if (!parentWinch) throw new Error('parent did not receive SIGWINCH');",
            "console.log('nested-winch-complete');",
        ]
        .join("\n"),
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-pty-resize-nested-sigwinch");
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
        std::fs::read_to_string(&child_entry).expect("read PTY child fixture"),
    );
    let ownership = wire_vm(&connection_id, &session_id, &vm_id);
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            ownership.clone(),
            RequestPayload::ExecuteRequest(agentos_native_sidecar::wire::ExecuteRequest {
                process_id: String::from("pty-winch-parent"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(parent_entry.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: HashMap::from([(String::from("AGENTOS_EXEC_TTY"), String::from("1"))]),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("start PTY parent");
    assert!(matches!(
        started.response.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));

    wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "pty-winch-parent",
        "child-winch-ready",
    );
    sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            ownership,
            RequestPayload::ResizePtyRequest(ResizePtyRequest {
                process_id: String::from("pty-winch-parent"),
                cols: 132,
                rows: 48,
            }),
        ))
        .expect("resize parent PTY");

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "pty-winch-parent",
        Duration::from_secs(10),
    );
    assert_eq!(exit_code, 0, "PTY parent failed: {stderr}");
    assert!(
        stdout.contains("parent-winch"),
        "root runtime missed SIGWINCH: {stdout}"
    );
    assert!(
        stdout.contains("child-winch"),
        "nested foreground runtime missed SIGWINCH: {stdout}"
    );
    assert!(
        stdout.contains("nested-winch-complete"),
        "parent did not observe nested signal completion: {stdout}"
    );
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-embedded-runtime-signal-sigchld");
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
        std::fs::read_to_string(&child_entry).expect("read SIGCHLD child fixture"),
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
            .expect("query SIGCHLD state");
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
fn embedded_runtime_signal_suite() {
    embedded_runtime_signal_routes_sigterm_and_process_kill();
    embedded_runtime_signal_stop_continue_updates_kernel_state_and_guest_handler();
    embedded_runtime_kill_process_rejects_invalid_signal_without_killing_process();
    embedded_runtime_process_kill_signal_zero_checks_child_liveness();
    embedded_runtime_process_group_kill_terminates_detached_tree();
    pty_resize_delivers_sigwinch_to_nested_foreground_runtime();
    embedded_runtime_signal_delivers_sigchld_on_child_exit();
}
