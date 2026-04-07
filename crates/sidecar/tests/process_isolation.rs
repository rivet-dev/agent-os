mod support;

use agent_os_sidecar::protocol::{EventPayload, GuestRuntimeKind, OwnershipScope, StreamChannel};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, create_vm, execute, new_sidecar, open_session, temp_dir,
    write_fixture,
};

#[derive(Debug, Default)]
struct ProcessResult {
    stderr: String,
    exit_code: Option<i32>,
}

#[test]
fn concurrent_vm_processes_stay_isolated_with_vm_scoped_events() {
    assert_node_available();

    let mut sidecar = new_sidecar("process-isolation");
    let cwd = temp_dir("process-isolation-cwd");
    let slow_entry = cwd.join("slow.cjs");
    let fast_entry = cwd.join("fast.cjs");

    write_fixture(
        &slow_entry,
        "setTimeout(() => {}, 150);\n",
    );
    write_fixture(&fast_entry, "void 0;\n");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (slow_vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (fast_vm_id, _) = create_vm(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute(
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
    execute(
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
    let ownership = OwnershipScope::session(&connection_id, &session_id);

    while results.values().any(|result| result.exit_code.is_none()) {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll process-isolation event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for isolated process events"
            );
            continue;
        };

        let OwnershipScope::Vm { vm_id, .. } = event.ownership else {
            panic!("expected VM-scoped process event");
        };
        let result = results
            .get_mut(&vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {vm_id}"));

        match event.payload {
            EventPayload::ProcessOutput(output) => match output.channel {
                StreamChannel::Stdout => {}
                StreamChannel::Stderr => result.stderr.push_str(&output.chunk),
            },
            EventPayload::ProcessExited(exited) => {
                assert_eq!(exited.process_id, "proc");
                result.exit_code = Some(exited.exit_code);
            }
            _ => {}
        }
    }

    let slow = results.get(&slow_vm_id).expect("slow vm result");
    let fast = results.get(&fast_vm_id).expect("fast vm result");

    assert_eq!(slow.exit_code, Some(0));
    assert_eq!(fast.exit_code, Some(0));
    assert!(
        slow.stderr.is_empty(),
        "unexpected slow stderr: {}",
        slow.stderr
    );
    assert!(
        fast.stderr.is_empty(),
        "unexpected fast stderr: {}",
        fast.stderr
    );
}
