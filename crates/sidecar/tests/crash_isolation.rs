mod support;

use agent_os_sidecar::protocol::{EventPayload, GuestRuntimeKind, OwnershipScope, StreamChannel};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm, execute, new_sidecar,
    open_session, temp_dir, write_fixture,
};

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
    let crash_entry = cwd.join("crash.mjs");
    let healthy_entry = cwd.join("healthy.mjs");

    write_fixture(&crash_entry, "throw new Error(\"boom\");\n");
    write_fixture(&healthy_entry, "console.log(\"healthy\");\n");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (crash_vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (healthy_vm_id, _) = create_vm(
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
        &crash_vm_id,
        "proc-crash",
        GuestRuntimeKind::JavaScript,
        &crash_entry,
        Vec::new(),
    );
    execute(
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
    let ownership = OwnershipScope::session(&connection_id, &session_id);

    while results.values().any(|result| result.exit_code.is_none()) {
        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll crash-isolation event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for crash-isolation events"
            );
            continue;
        };

        let OwnershipScope::Vm { vm_id, .. } = event.ownership else {
            panic!("expected VM-scoped crash-isolation event");
        };
        let result = results
            .get_mut(&vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {vm_id}"));

        match event.payload {
            EventPayload::ProcessOutput(output) => match output.channel {
                StreamChannel::Stdout => result.stdout.push_str(&output.chunk),
                StreamChannel::Stderr => result.stderr.push_str(&output.chunk),
            },
            EventPayload::ProcessExited(exited) => {
                result.exit_code = Some(exited.exit_code);
            }
            _ => {}
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
    assert_eq!(healthy.stdout.trim(), "healthy");
    assert!(
        healthy.stderr.is_empty(),
        "unexpected healthy stderr: {}",
        healthy.stderr
    );

    execute(
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
    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &healthy_vm_id,
        "proc-healthy-2",
    );

    assert_eq!(exit_code, 0);
    assert_eq!(stdout.trim(), "healthy");
    assert!(stderr.is_empty(), "unexpected follow-up stderr: {stderr}");
}
