mod support;

use agent_os_bridge::{LoadFilesystemStateRequest, PersistenceBridge};
use agent_os_sidecar::protocol::{
    CreateVmRequest, DisposeReason, DisposeVmRequest, EventPayload, GuestRuntimeKind,
    KillProcessRequest, OpenSessionRequest, OwnershipScope, RequestPayload, ResponsePayload,
    SidecarPlacement,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm, execute, new_sidecar,
    open_session, request, temp_dir, write_fixture, RecordingBridge,
};

fn wait_for_process_exit(
    sidecar: &mut agent_os_sidecar::NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> i32 {
    let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar process exit");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process exit"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                return exited.exit_code;
            }
            _ => {}
        }
    }
}

#[test]
fn kill_process_terminates_running_guest_execution() {
    assert_node_available();

    let mut sidecar = new_sidecar("kill-process");
    let cwd = temp_dir("kill-process-cwd");
    let entry = cwd.join("hang.mjs");
    write_fixture(&entry, "setInterval(() => {}, 1000);\n");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let kill = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("proc-hang"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill guest process");

    match kill.response.payload {
        ResponsePayload::ProcessKilled(response) => {
            assert_eq!(response.process_id, "proc-hang");
        }
        other => panic!("unexpected kill response: {other:?}"),
    }

    let exit_code = wait_for_process_exit(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
    );
    assert_ne!(exit_code, 0);

    let rerun = cwd.join("rerun.mjs");
    write_fixture(&rerun, "console.log('rerun-ok');\n");
    execute(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-rerun",
        GuestRuntimeKind::JavaScript,
        &rerun,
        Vec::new(),
    );
    let (_stdout, stderr, rerun_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-rerun",
    );
    assert!(stderr.is_empty());
    assert_eq!(rerun_exit, 0);
}

#[test]
fn dispose_vm_succeeds_even_when_a_guest_process_is_running() {
    assert_node_available();

    let mut sidecar = new_sidecar("dispose-vm-running-process");
    let cwd = temp_dir("dispose-vm-running-process-cwd");
    let entry = cwd.join("hang.mjs");
    write_fixture(&entry, "setInterval(() => {}, 1000);\n");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-hang",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let dispose = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVm(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose vm with running process");

    match dispose.response.payload {
        ResponsePayload::VmDisposed(response) => {
            assert_eq!(response.vm_id, vm_id);
        }
        other => panic!("unexpected dispose response: {other:?}"),
    }
    assert!(dispose
        .events
        .iter()
        .any(|event| matches!(event.payload, EventPayload::ProcessExited(_))));

    let replacement_vm = sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem: Default::default(),
                permissions: None,
            }),
        ))
        .expect("create replacement vm after dispose");
    match replacement_vm.response.payload {
        ResponsePayload::VmCreated(_) => {}
        other => panic!("unexpected replacement vm response: {other:?}"),
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
                "disposed vm should flush a filesystem snapshot"
            );
        })
        .expect("inspect persistence bridge");
}

#[test]
fn close_session_removes_the_session_and_disposes_owned_vms() {
    let mut sidecar = new_sidecar("close-session");
    let cwd = temp_dir("close-session-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let events = sidecar
        .close_session_blocking(&connection_id, &session_id)
        .expect("close owned session");
    assert!(events.iter().any(|event| {
        matches!(
            event.payload,
            EventPayload::VmLifecycle(agent_os_sidecar::protocol::VmLifecycleEvent {
                state: agent_os_sidecar::protocol::VmLifecycleState::Disposed,
            })
        )
    }));

    let create_after_close = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem: Default::default(),
                permissions: None,
            }),
        ))
        .expect("dispatch closed-session create_vm");
    match create_after_close.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains("unknown sidecar session"));
        }
        other => panic!("unexpected closed-session create_vm response: {other:?}"),
    }

    let reopened = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::connection(&connection_id),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: SidecarPlacement::Shared { pool: None },
                metadata: BTreeMap::new(),
            }),
        ))
        .expect("open replacement session");
    match reopened.response.payload {
        ResponsePayload::SessionOpened(_) => {}
        other => panic!("unexpected session reopen response: {other:?}"),
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
                "closing a session should dispose its VMs"
            );
        })
        .expect("inspect persistence bridge");
}

#[test]
fn remove_connection_disposes_owned_sessions_and_vms() {
    let mut sidecar = new_sidecar("remove-connection");
    let cwd = temp_dir("remove-connection-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let events = sidecar
        .remove_connection_blocking(&connection_id)
        .expect("remove authenticated connection");
    assert!(events.iter().any(|event| {
        matches!(
            event.payload,
            EventPayload::VmLifecycle(agent_os_sidecar::protocol::VmLifecycleEvent {
                state: agent_os_sidecar::protocol::VmLifecycleState::Disposed,
            })
        )
    }));

    let reopened = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::connection(&connection_id),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: SidecarPlacement::Shared { pool: None },
                metadata: BTreeMap::new(),
            }),
        ))
        .expect("attempt open session after connection removal");
    match reopened.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains("has not authenticated"));
        }
        other => panic!("unexpected post-removal open-session response: {other:?}"),
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
                "removing a connection should dispose its VMs"
            );
        })
        .expect("inspect persistence bridge");
}
