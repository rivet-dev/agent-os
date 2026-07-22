mod support;

use agentos_native_sidecar::wire::{EventPayload, GuestRuntimeKind, OwnershipScope, StreamChannel};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, execute_wire, new_sidecar,
    open_session_wire, temp_dir, wire_session, wire_vm, write_fixture,
};

const PROCESS_OUTPUT_BYTE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct ProcessResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

#[test]
fn guest_failure_in_one_vm_does_not_break_peer_vm_execution() {
    assert_node_available();

    let mut sidecar = new_sidecar("crash-isolation");
    let cwd = temp_dir("crash-isolation-cwd");
    let crash_entry = cwd.join("crash.cjs");
    let healthy_entry = cwd.join("healthy.cjs");

    write_fixture(&crash_entry, "throw new Error(\"boom\");\n");
    write_fixture(&healthy_entry, "console.log(\"healthy\");\n");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (crash_vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (healthy_vm_id, _) = create_vm_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &crash_vm_id,
        "proc-crash",
        GuestRuntimeKind::JavaScript,
        &crash_entry,
        Vec::new(),
    );
    execute_wire(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &healthy_vm_id,
        "proc-healthy",
        GuestRuntimeKind::JavaScript,
        &healthy_entry,
        Vec::new(),
    );

    let mut results = BTreeMap::from([
        (crash_vm_id.clone(), ProcessResult::default()),
        (healthy_vm_id.clone(), ProcessResult::default()),
    ]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let ownership = wire_session(&connection_id, &session_id);

    let is_complete = |results: &BTreeMap<String, ProcessResult>| {
        let crash = results
            .get(&crash_vm_id)
            .expect("crash vm result should exist");
        let healthy = results
            .get(&healthy_vm_id)
            .expect("healthy vm result should exist");

        crash.exit_code == Some(1) && healthy.exit_code == Some(0)
    };

    while !is_complete(&results) {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll crash-isolation event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for crash-isolation events"
            );
            continue;
        };

        let OwnershipScope::VmOwnership(vm_ownership) = event.ownership else {
            panic!("expected VM-scoped crash-isolation event");
        };
        let result = results
            .get_mut(&vm_ownership.vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {}", vm_ownership.vm_id));

        match event.payload {
            EventPayload::ProcessOutputEvent(output) => match output.channel {
                StreamChannel::Stdout => {
                    append_process_output(
                        &mut result.stdout,
                        &output.chunk,
                        &output.process_id,
                        "stdout",
                    );
                }
                StreamChannel::Stderr => {
                    append_process_output(
                        &mut result.stderr,
                        &output.chunk,
                        &output.process_id,
                        "stderr",
                    );
                }
            },
            EventPayload::ProcessExitedEvent(exited) => {
                result.exit_code = Some(exited.exit_code);
            }
            EventPayload::ExecutionOutputEvent(_)
            | EventPayload::ExecutionCompletedEvent(_)
            | EventPayload::VmLifecycleEvent(_)
            | EventPayload::StructuredEvent(_)
            | EventPayload::ExtEnvelope(_) => {}
        }
    }

    let crash = results.get(&crash_vm_id).expect("crash vm result");
    let healthy = results.get(&healthy_vm_id).expect("healthy vm result");

    assert_eq!(crash.exit_code, Some(1));
    assert!(
        crash.stderr.contains("boom"),
        "unexpected crash stderr: {}",
        crash.stderr
    );
    assert_eq!(healthy.exit_code, Some(0));
    assert!(
        healthy.stderr.is_empty(),
        "unexpected healthy stderr: {}",
        healthy.stderr
    );

    execute_wire(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &healthy_vm_id,
        "proc-healthy-2",
        GuestRuntimeKind::JavaScript,
        &healthy_entry,
        Vec::new(),
    );
    let (_stdout, stderr, exit_code) = collect_crash_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &healthy_vm_id,
        "proc-healthy-2",
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected follow-up stderr: {stderr}");
}

fn collect_crash_process_output(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> (String, String, i32) {
    let ownership = wire_session(connection_id, session_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut exit = None;

    loop {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll crash-isolation follow-up event");
        if let Some(event) = event {
            assert_eq!(event.ownership, wire_vm(connection_id, session_id, vm_id));

            match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => append_process_output(
                            &mut stdout,
                            &output.chunk,
                            &output.process_id,
                            "stdout",
                        ),
                        StreamChannel::Stderr => append_process_output(
                            &mut stderr,
                            &output.chunk,
                            &output.process_id,
                            "stderr",
                        ),
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    exit = Some((exited.exit_code, Instant::now()));
                }
                EventPayload::ProcessOutputEvent(_)
                | EventPayload::ProcessExitedEvent(_)
                | EventPayload::ExecutionOutputEvent(_)
                | EventPayload::ExecutionCompletedEvent(_)
                | EventPayload::VmLifecycleEvent(_)
                | EventPayload::StructuredEvent(_)
                | EventPayload::ExtEnvelope(_) => {}
            }
        }

        if let Some((exit_code, seen_at)) = exit {
            if Instant::now().duration_since(seen_at) >= Duration::from_millis(200) {
                return (stdout, stderr, exit_code);
            }
        }

        assert!(
            Instant::now() < deadline,
            "timed out waiting for crash-isolation process {process_id}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

fn append_process_output(buffer: &mut String, chunk: &[u8], process_id: &str, channel: &str) {
    let text = String::from_utf8_lossy(chunk);
    assert!(
        buffer.len().saturating_add(text.len()) <= PROCESS_OUTPUT_BYTE_LIMIT,
        "crash-isolation process {process_id} exceeded {PROCESS_OUTPUT_BYTE_LIMIT} bytes on {channel}"
    );
    buffer.push_str(&text);
}
