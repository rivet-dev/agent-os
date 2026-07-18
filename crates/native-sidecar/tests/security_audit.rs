mod support;

use agentos_bridge::StructuredEventRecord;
use agentos_native_sidecar::wire::{
    BootstrapRootFilesystemRequest, ConfigureVmRequest, FsPermissionRule, FsPermissionRuleSet,
    FsPermissionScope, GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind,
    KillProcessRequest, MountDescriptor, MountPluginDescriptor, PermissionMode, PermissionsPolicy,
    RequestPayload, ResponsePayload, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind,
};
use std::collections::HashMap;
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, authenticate_wire_with_token,
    collect_process_output_wire_with_timeout, create_vm_wire, execute_wire, open_session_wire,
    temp_dir, wire_request, wire_vm, write_fixture, RecordingBridge,
};

fn structured_events(
    sidecar: &agentos_native_sidecar::NativeSidecar<RecordingBridge>,
) -> Vec<StructuredEventRecord> {
    sidecar
        .with_bridge_mut(|bridge| bridge.structured_events.clone())
        .expect("inspect structured events")
}

fn find_event<'a>(events: &'a [StructuredEventRecord], name: &str) -> &'a StructuredEventRecord {
    events
        .iter()
        .find(|event| event.name == name)
        .unwrap_or_else(|| panic!("missing structured event: {name}"))
}

fn assert_timestamp(event: &StructuredEventRecord) {
    event.fields["timestamp"]
        .parse::<u128>()
        .unwrap_or_else(|error| panic!("invalid audit timestamp: {error}"));
}

fn wait_for_process_exit_bounded(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> i32 {
    let (_, _, exit_code) = collect_process_output_wire_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        Duration::from_secs(10),
    );
    exit_code
}

#[test]
fn auth_failures_emit_security_audit_events() {
    let mut sidecar = support::new_sidecar("security-audit-auth");

    let result = authenticate_wire_with_token(&mut sidecar, 1, "conn-hint", "wrong-token");
    match result.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "unauthorized");
            assert!(rejected.message.contains("invalid auth token"));
        }
        other => panic!("unexpected auth failure response: {other:?}"),
    }

    let events = structured_events(&sidecar);
    let event = find_event(&events, "security.auth.failed");
    assert_eq!(event.vm_id, "sidecar-security-audit-auth");
    assert_eq!(event.fields["source"], "sidecar-tests");
    assert_eq!(event.fields["connection_id"], "conn-hint");
    assert!(event.fields["reason"].contains("invalid auth token"));
    assert_timestamp(event);
}

#[test]
fn filesystem_permission_denials_emit_security_audit_events() {
    let mut sidecar = support::new_sidecar("security-audit-permissions");
    let cwd = temp_dir("security-audit-permissions-cwd");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let denied_vm_id = vm_id.clone();
    let sidecar = &mut sidecar;
    let _ = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::FsPermissionRuleSet(
                        FsPermissionRuleSet {
                            default: Some(PermissionMode::Allow),
                            rules: vec![FsPermissionRule {
                                mode: PermissionMode::Deny,
                                operations: vec![String::from("read")],
                                paths: vec![String::from("/blocked.txt")],
                            }],
                        },
                    )),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    binding: None,
                }),
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
        .expect("configure vm permissions");

    let write = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &denied_vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/blocked.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("blocked")),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                recursive: false,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        ))
        .expect("write blocked file");
    match write.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(_) => {}
        other => panic!("unexpected write response: {other:?}"),
    }

    let read = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &denied_vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/blocked.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        ))
        .expect("dispatch denied read");
    match read.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "EACCES");
            assert!(rejected.message.contains("EACCES"));
        }
        other => panic!("unexpected read response: {other:?}"),
    }

    let events = structured_events(sidecar);
    let event = find_event(&events, "security.permission.denied");
    assert_eq!(event.vm_id, denied_vm_id);
    assert_eq!(event.fields["operation"], "read");
    assert_eq!(event.fields["path"], "/blocked.txt");
    assert_eq!(event.fields["policy"], "fs.read");
    assert!(event.fields["reason"].contains("fs.read"));
    assert_timestamp(event);
}

#[test]
fn mount_operations_emit_security_audit_events() {
    let mut sidecar = support::new_sidecar("security-audit-mounts");
    let cwd = temp_dir("security-audit-mounts-cwd");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::BootstrapRootFilesystemRequest(BootstrapRootFilesystemRequest {
                entries: vec![RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: None,
                    encoding: None,
                    target: None,
                    executable: false,
                }],
            }),
        ))
        .expect("bootstrap workspace");

    sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    guest_source: String::from("memory"),
                    guest_fstype: String::from("memory"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("memory"),
                        config: serde_json::to_string(&serde_json::json!({}))
                            .expect("serialize memory mount config"),
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
        .expect("mount workspace");

    sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
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
        .expect("unmount workspace");

    let events = structured_events(&sidecar);
    // VM creation auto-mounts /opt/agentos (package projection) first; find
    // the audit event for THIS test's explicit /workspace mount.
    let mounted = events
        .iter()
        .find(|event| {
            event.name == "security.mount.mounted"
                && event.fields.get("guest_path").map(String::as_str) == Some("/workspace")
        })
        .expect("missing /workspace mount audit event");
    assert_eq!(mounted.vm_id, vm_id);
    assert_eq!(mounted.fields["guest_path"], "/workspace");
    assert_eq!(mounted.fields["plugin_id"], "memory");
    assert_eq!(mounted.fields["read_only"], "false");
    assert_timestamp(mounted);

    let unmounted = events
        .iter()
        .rfind(|event| {
            event.name == "security.mount.unmounted"
                && event.fields.get("guest_path").map(String::as_str) == Some("/workspace")
        })
        .expect("missing /workspace unmount audit event");
    assert_eq!(unmounted.vm_id, vm_id);
    assert_eq!(unmounted.fields["guest_path"], "/workspace");
    assert_eq!(unmounted.fields["plugin_id"], "memory");
    assert_eq!(unmounted.fields["read_only"], "false");
    assert_timestamp(unmounted);
}

#[test]
fn kill_requests_emit_security_audit_events() {
    assert_node_available();

    let mut sidecar = support::new_sidecar("security-audit-kill");
    let cwd = temp_dir("security-audit-kill-cwd");
    let entry = cwd.join("sleep.cjs");
    write_fixture(
        &entry,
        "setInterval(() => { process.stdout.write('tick\\n'); }, 1000);\n",
    );

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-kill",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: String::from("proc-kill"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill js process");
    match result.response.payload {
        ResponsePayload::ProcessKilledResponse(_) => {}
        other => panic!("unexpected kill response: {other:?}"),
    }

    let exit_code = wait_for_process_exit_bounded(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-kill",
    );
    assert_eq!(exit_code, 143);

    let events = structured_events(&sidecar);
    let event = find_event(&events, "security.process.kill");
    assert_eq!(event.vm_id, vm_id);
    assert_eq!(event.fields["source"], "control_plane");
    assert_eq!(event.fields["source_pid"], "0");
    assert_eq!(event.fields["process_id"], "proc-kill");
    assert_eq!(event.fields["signal"], "SIGTERM");
    assert!(event.fields.contains_key("target_pid"));
    assert!(event.fields.contains_key("host_pid"));
    assert_timestamp(event);
}
