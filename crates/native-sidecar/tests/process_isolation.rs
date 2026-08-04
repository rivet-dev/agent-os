mod support;

use agentos_native_sidecar::wire::{EventPayload, GuestRuntimeKind, OwnershipScope, StreamChannel};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, execute_wire, new_sidecar,
    open_session_wire, temp_dir, wire_session, write_fixture,
};

const MAX_PROCESS_STDERR_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct ProcessResult {
    stderr: Vec<u8>,
    exit_code: Option<i32>,
}

fn append_stderr(result: &mut ProcessResult, chunk: &[u8]) {
    assert!(
        result.stderr.len().saturating_add(chunk.len()) <= MAX_PROCESS_STDERR_BYTES,
        "process stderr exceeded {MAX_PROCESS_STDERR_BYTES} bytes"
    );
    result.stderr.extend_from_slice(chunk);
}

#[test]
fn concurrent_vm_processes_stay_isolated_with_vm_scoped_events() {
    assert_node_available();

    let mut sidecar = new_sidecar("process-isolation");
    let cwd = temp_dir("process-isolation-cwd");
    let slow_entry = cwd.join("slow.cjs");
    let fast_entry = cwd.join("fast.cjs");

    write_fixture(&slow_entry, "setTimeout(() => {}, 150);\n");
    write_fixture(&fast_entry, "void 0;\n");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (slow_vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (fast_vm_id, _) = create_vm_wire(
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
        &slow_vm_id,
        "proc",
        GuestRuntimeKind::JavaScript,
        &slow_entry,
        Vec::new(),
    );
    execute_wire(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &fast_vm_id,
        "proc",
        GuestRuntimeKind::JavaScript,
        &fast_entry,
        Vec::new(),
    );

    let mut results = BTreeMap::from([
        (slow_vm_id.clone(), ProcessResult::default()),
        (fast_vm_id.clone(), ProcessResult::default()),
    ]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let ownership = wire_session(&connection_id, &session_id);

    while results.values().any(|result| result.exit_code.is_none()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for isolated process events"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll process-isolation event");
        let Some(event) = event else { continue };

        let OwnershipScope::VmOwnership(vm_ownership) = event.ownership else {
            panic!("expected VM-scoped process event");
        };
        let result = results
            .get_mut(&vm_ownership.vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {}", vm_ownership.vm_id));

        match event.payload {
            EventPayload::ProcessOutputEvent(output) => {
                assert_eq!(output.process_id, "proc");
                match output.channel {
                    StreamChannel::Stdout => {}
                    StreamChannel::Stderr => {
                        append_stderr(result, &output.chunk);
                    }
                }
            }
            EventPayload::ProcessExitedEvent(exited) => {
                assert_eq!(exited.process_id, "proc");
                result.exit_code = Some(exited.exit_code);
            }
            EventPayload::VmLifecycleEvent(_)
            | EventPayload::StructuredEvent(_)
            | EventPayload::ExecutionOutputEvent(_)
            | EventPayload::ExecutionCompletedEvent(_)
            | EventPayload::ExtEnvelope(_) => {}
        }
    }

    let slow = results.get(&slow_vm_id).expect("slow vm result");
    let fast = results.get(&fast_vm_id).expect("fast vm result");

    assert_eq!(slow.exit_code, Some(0));
    assert_eq!(fast.exit_code, Some(0));
    let slow_stderr = String::from_utf8_lossy(&slow.stderr);
    let fast_stderr = String::from_utf8_lossy(&fast.stderr);
    assert!(
        slow_stderr.is_empty(),
        "unexpected slow stderr: {}",
        slow_stderr
    );
    assert!(
        fast_stderr.is_empty(),
        "unexpected fast stderr: {}",
        fast_stderr
    );
}
