mod support;

use agentos_native_sidecar::wire::{
    ConfigureVmRequest, CreateVmRequest, GuestRuntimeKind, MountDescriptor, MountPluginDescriptor,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, RootFilesystemMode,
};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    execute_wire, new_sidecar, open_session_wire, temp_dir, wire_permissions_allow_all,
    wire_request, wire_session, write_fixture,
};

#[test]
fn javascript_fs_read_options_overload_works_on_host_mounts() {
    assert_node_available();

    let mut sidecar = new_sidecar("fs-read-options-host-mount");
    let cwd = temp_dir("fs-read-options-host-mount-cwd");
    let entry = cwd.join("fs-read-options-host-mount.mjs");
    let host_dir = temp_dir("fs-read-options-host-mount-host");
    std::fs::write(host_dir.join("input.txt"), "abcdef").expect("seed host file");

    write_fixture(
        &entry,
        r#"
import fs from "node:fs";

const withBuffer = await new Promise((resolve, reject) => {
  fs.open("/workspace/input.txt", "r", (openError, fd) => {
    if (openError) return reject(openError);
    const buffer = Buffer.alloc(3);
    fs.read(fd, { buffer, position: 1 }, (readError, bytesRead, resultBuffer) => {
      fs.closeSync(fd);
      if (readError) return reject(readError);
      resolve({ bytesRead, text: resultBuffer.toString("utf8", 0, bytesRead) });
    });
  });
});

const allocated = await new Promise((resolve, reject) => {
  fs.open("/workspace/input.txt", "r", (openError, fd) => {
    if (openError) return reject(openError);
    fs.read(fd, { position: 2, length: 2 }, (readError, bytesRead, resultBuffer) => {
      fs.closeSync(fd);
      if (readError) return reject(readError);
      resolve({ bytesRead, text: resultBuffer.toString("utf8", 0, bytesRead) });
    });
  });
});

console.log(JSON.stringify({ withBuffer, allocated }));
"#,
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-fs-read-options-host-mount");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let create = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create sidecar vm");
    let vm_id = match create.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create vm response: {other:?}"),
    };

    let configure = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            support::wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    guest_source: String::from("host_dir"),
                    guest_fstype: String::from("host_dir"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: json!({
                            "hostPath": host_dir.to_string_lossy().into_owned(),
                            "readOnly": false,
                        })
                        .to_string(),
                    },
                }],
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: HashMap::new(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("configure host mount");
    assert!(matches!(
        configure.response.payload,
        ResponsePayload::VmConfiguredResponse(_)
    ));

    execute_wire(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-read-options-host-mount",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "fs-read-options-host-mount",
        Duration::from_secs(10),
    );
    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");
    let payload: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("parse fs read options result");
    assert_eq!(
        payload["withBuffer"],
        json!({ "bytesRead": 3, "text": "bcd" })
    );
    assert_eq!(
        payload["allocated"],
        json!({ "bytesRead": 2, "text": "cd" })
    );
}
