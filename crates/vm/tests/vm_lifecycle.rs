mod support;

use agentos_vm::wire::{
    BootstrapRootFilesystemRequest, DisposeReason, DisposeVmRequest, GuestRuntimeKind,
    RequestPayload, ResponsePayload, RootFilesystemEntry, RootFilesystemEntryKind,
};
use agentos_vm_host_interface::{HostPersistence, LoadFilesystemStateRequest};
use agentos_vm_kernel::root_fs::{
    decode_snapshot as decode_root_snapshot, ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_wire, execute_wire, new_sidecar, open_session_wire, temp_dir, wasm_stdout_module,
    wire_request, wire_vm, write_fixture,
};

fn root_entry(path: &str, kind: RootFilesystemEntryKind, executable: bool) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.to_owned(),
        kind,
        mode: None,
        uid: None,
        gid: None,
        content: None,
        encoding: None,
        target: None,
        executable,
    }
}

#[test]
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);

    let (js_vm_id, js_create) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    assert_eq!(js_create.events.len(), 2);

    let bootstrap = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &js_vm_id),
            RequestPayload::BootstrapRootFilesystemRequest(BootstrapRootFilesystemRequest {
                entries: vec![
                    root_entry("/workspace", RootFilesystemEntryKind::Directory, false),
                    root_entry("/workspace/run.sh", RootFilesystemEntryKind::File, true),
                ],
            }),
        ))
        .expect("bootstrap root filesystem");
    match bootstrap.response.payload {
        ResponsePayload::RootFilesystemBootstrappedResponse(response) => {
            assert_eq!(response.entry_count, 2);
        }
        other => panic!("unexpected bootstrap response: {other:?}"),
    }

    execute_wire(
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
    let (js_stdout, js_stderr, js_exit) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &js_vm_id,
        "proc-js",
        Duration::from_secs(10),
    );
    assert_eq!(js_stdout.trim(), "js:alpha,beta");
    assert!(js_stderr.is_empty());
    assert_eq!(js_exit, 0);

    let (wasm_vm_id, _) = create_vm_wire(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );
    execute_wire(
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
    let (wasm_stdout, wasm_stderr, wasm_exit) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &wasm_vm_id,
        "proc-wasm",
        Duration::from_secs(10),
    );
    assert_eq!(wasm_stdout.trim(), "wasm:ready");
    assert!(wasm_stderr.is_empty());
    assert_eq!(wasm_exit, 0);

    sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &js_vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose js vm");
    sidecar
        .dispatch_wire_blocking(wire_request(
            9,
            wire_vm(&connection_id, &session_id, &wasm_vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose wasm vm");

    sidecar
        .with_bridge_mut(|bridge: &mut support::RecordingBridge| {
            let command_checks = bridge
                .permission_checks
                .iter()
                .filter(|check| check.starts_with("cmd:"))
                .collect::<Vec<_>>();
            if !command_checks.is_empty() {
                assert!(command_checks.iter().any(|check| {
                    *check == &format!("cmd:{js_vm_id}:node")
                        || *check == &format!("cmd:{wasm_vm_id}:wasm")
                }));
            }
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
            let wasm_root =
                decode_root_snapshot(&wasm_snapshot.bytes).expect("decode wasm root snapshot");
            assert!(!wasm_root
                .entries
                .iter()
                .any(|entry| entry.path == "/workspace/run.sh"));
            assert!(bridge.lifecycle_events.iter().any(|event| {
                event.vm_id == js_vm_id
                    && event.state == agentos_vm_host_interface::LifecycleState::Busy
            }));
        })
        .expect("inspect bridge");
}
