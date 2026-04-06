mod support;

use agent_os_bridge::{LoadFilesystemStateRequest, PersistenceBridge};
use agent_os_kernel::root_fs::{
    decode_snapshot as decode_root_snapshot, ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use agent_os_sidecar::protocol::{
    BootstrapRootFilesystemRequest, GuestRuntimeKind, OwnershipScope, RequestPayload,
    ResponsePayload, RootFilesystemEntry, RootFilesystemEntryKind,
};
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm, execute, new_sidecar,
    open_session, request, temp_dir, wasm_stdout_module, write_fixture,
};

#[test]
#[ignore]
fn native_sidecar_composes_vm_lifecycle_bridge_callbacks_and_guest_execution() {
    assert_node_available();

    let mut sidecar = new_sidecar("vm-lifecycle");
    let cwd = temp_dir("vm-lifecycle-cwd");
    let js_entry = cwd.join("entry.mjs");
    let wasm_entry = cwd.join("entry.wasm");

    write_fixture(
        &js_entry,
        r#"
console.log(`js:${process.argv.slice(2).join(",")}`);
"#,
    );
    write_fixture(&wasm_entry, wasm_stdout_module());

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);

    let (js_vm_id, js_create) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    assert_eq!(js_create.events.len(), 2);

    let bootstrap = sidecar
        .dispatch(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &js_vm_id),
            RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/run.sh"),
                        kind: RootFilesystemEntryKind::File,
                        executable: true,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("bootstrap root filesystem");
    match bootstrap.response.payload {
        ResponsePayload::RootFilesystemBootstrapped(response) => {
            assert_eq!(response.entry_count, 2);
        }
        other => panic!("unexpected bootstrap response: {other:?}"),
    }

    execute(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &js_vm_id,
        "proc-js",
        GuestRuntimeKind::JavaScript,
        &js_entry,
        vec![String::from("alpha"), String::from("beta")],
    );
    let (js_stdout, js_stderr, js_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &js_vm_id,
        "proc-js",
    );
    assert_eq!(js_stdout.trim(), "js:alpha,beta");
    assert!(js_stderr.is_empty());
    assert_eq!(js_exit, 0);

    let (wasm_vm_id, _) = create_vm(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );
    execute(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &wasm_vm_id,
        "proc-wasm",
        GuestRuntimeKind::WebAssembly,
        &wasm_entry,
        Vec::new(),
    );
    let (wasm_stdout, wasm_stderr, wasm_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &wasm_vm_id,
        "proc-wasm",
    );
    assert_eq!(wasm_stdout.trim(), "wasm:ready");
    assert!(wasm_stderr.is_empty());
    assert_eq!(wasm_exit, 0);

    sidecar
        .dispatch(request(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &js_vm_id),
            RequestPayload::DisposeVm(agent_os_sidecar::protocol::DisposeVmRequest {
                reason: agent_os_sidecar::protocol::DisposeReason::Requested,
            }),
        ))
        .expect("dispose js vm");
    sidecar
        .dispatch(request(
            9,
            OwnershipScope::vm(&connection_id, &session_id, &wasm_vm_id),
            RequestPayload::DisposeVm(agent_os_sidecar::protocol::DisposeVmRequest {
                reason: agent_os_sidecar::protocol::DisposeReason::Requested,
            }),
        ))
        .expect("dispose wasm vm");

    sidecar
        .with_bridge_mut(|bridge: &mut support::RecordingBridge| {
            assert!(bridge.permission_checks.iter().any(|check| {
                check == &format!("cmd:{js_vm_id}:node")
                    || check == &format!("cmd:{wasm_vm_id}:wasm")
            }));
            let js_snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: js_vm_id.clone(),
                })
                .expect("load js snapshot")
                .expect("persisted js snapshot");
            assert_eq!(js_snapshot.format, ROOT_FILESYSTEM_SNAPSHOT_FORMAT);
            let js_root =
                decode_root_snapshot(&js_snapshot.bytes).expect("decode js root snapshot");
            assert!(js_root
                .entries
                .iter()
                .any(|entry| entry.path == "/bin/node"));
            assert!(js_root
                .entries
                .iter()
                .any(|entry| entry.path == "/workspace/run.sh"));

            let wasm_snapshot = bridge
                .load_filesystem_state(LoadFilesystemStateRequest {
                    vm_id: wasm_vm_id.clone(),
                })
                .expect("load wasm snapshot")
                .expect("persisted wasm snapshot");
            assert_eq!(wasm_snapshot.format, ROOT_FILESYSTEM_SNAPSHOT_FORMAT);
            assert!(bridge.lifecycle_events.iter().any(|event| {
                event.vm_id == js_vm_id && event.state == agent_os_bridge::LifecycleState::Busy
            }));
        })
        .expect("inspect bridge");
}
