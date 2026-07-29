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
fn exact_node_eval_child_resolves_relative_imports_from_cwd() {
    assert_node_available();

    let mut sidecar = new_sidecar("exact-node-eval-relative-import");
    let cwd = temp_dir("exact-node-eval-relative-import-cwd");
    let entry = cwd.join("parent.mjs");
    let workspace = temp_dir("exact-node-eval-relative-import-workspace");
    std::fs::write(
        workspace.join("value.mjs"),
        "export const value = 'relative-import-ok';\n",
    )
    .expect("seed child import");
    write_fixture(
        &entry,
        r#"
import { spawnSync } from "node:child_process";

const child = spawnSync(
  "/bin/node",
  ["-e", "import('./value.mjs').then(({ value }) => console.log(value))"],
  { cwd: "/workspace", encoding: "utf8" },
);
if (child.error || child.status !== 0) {
  throw new Error(JSON.stringify({
    error: child.error?.message,
    status: child.status,
    signal: child.signal,
    stdout: child.stdout,
    stderr: child.stderr,
  }));
}
console.log(child.stdout.trim());
"#,
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-exact-node-eval-relative-import");
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
                            "hostPath": workspace.to_string_lossy().into_owned(),
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
        .expect("configure workspace mount");
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
        "exact-node-eval-relative-import",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "exact-node-eval-relative-import",
        Duration::from_secs(10),
    );
    assert_eq!(exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.trim().is_empty(), "unexpected stderr:\n{stderr}");
    assert_eq!(stdout.trim(), "relative-import-ok");
}
