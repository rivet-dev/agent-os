mod support;

use agent_os_bridge::StructuredEventRecord;
use agent_os_sidecar::protocol::{
    BootstrapRootFilesystemRequest, ConfigureVmRequest, ExecuteRequest, FsPermissionRuleSet,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, KillProcessRequest,
    MountDescriptor, MountPluginDescriptor, OwnershipScope, PermissionMode, PermissionsPolicy,
    RequestPayload, ResponsePayload, RootFilesystemEntry, RootFilesystemEntryKind,
};
use support::{
    assert_node_available, authenticate, authenticate_with_token, collect_process_output,
    create_vm, open_session, request, temp_dir, write_fixture, RecordingBridge,
};

fn structured_events(
    sidecar: &agent_os_sidecar::NativeSidecar<RecordingBridge>,
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

#[test]
fn auth_failures_emit_security_audit_events() {
    let mut sidecar = support::new_sidecar("security-audit-auth");

    let result = authenticate_with_token(&mut sidecar, 1, "conn-hint", "wrong-token");
    match result.response.payload {
        ResponsePayload::Rejected(rejected) => {
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

    let denied_vm_id = vm_id.clone();
    let sidecar = &mut sidecar;
    let _ = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(agent_os_sidecar::protocol::FsPermissionScope::Rules(
                        FsPermissionRuleSet {
                            default: Some(PermissionMode::Allow),
                            rules: vec![agent_os_sidecar::protocol::FsPermissionRule {
                                mode: PermissionMode::Deny,
                                operations: vec![String::from("read")],
                                paths: Vec::new(),
                            }],
                        },
                    )),
                    network: None,
                    child_process: None,
                    env: None,
                }),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("configure vm permissions");

    let write = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &denied_vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/blocked.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("blocked")),
                encoding: Some(agent_os_sidecar::protocol::RootFilesystemEntryEncoding::Utf8),
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ))
        .expect("write blocked file");
    match write.response.payload {
        ResponsePayload::GuestFilesystemResult(_) => {}
        other => panic!("unexpected write response: {other:?}"),
    }

    let read = sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &denied_vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/blocked.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ))
        .expect("dispatch denied read");
    match read.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "kernel_error");
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

    sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest {
                entries: vec![RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    ..Default::default()
                }],
            }),
        ))
        .expect("bootstrap workspace");

    sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("memory"),
                        config: serde_json::json!({}),
                    },
                }],
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("mount workspace");

    sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("unmount workspace");

    let events = structured_events(&sidecar);
    let mounted = find_event(&events, "security.mount.mounted");
    assert_eq!(mounted.vm_id, vm_id);
    assert_eq!(mounted.fields["guest_path"], "/workspace");
    assert_eq!(mounted.fields["plugin_id"], "memory");
    assert_eq!(mounted.fields["read_only"], "false");
    assert_timestamp(mounted);

    let unmounted = events
        .iter()
        .rfind(|event| event.name == "security.mount.unmounted")
        .expect("missing unmount audit event");
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

    let result = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: String::from("proc-kill"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(entry.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: Default::default(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("start js process");
    match result.response.payload {
        ResponsePayload::ProcessStarted(_) => {}
        other => panic!("unexpected execute response: {other:?}"),
    }

    let result = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: String::from("proc-kill"),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill js process");
    match result.response.payload {
        ResponsePayload::ProcessKilled(_) => {}
        other => panic!("unexpected kill response: {other:?}"),
    }

    let (_stdout, _stderr, _exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-kill",
    );

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
