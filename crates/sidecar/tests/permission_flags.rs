mod support;

use agent_os_sidecar::protocol::{
    ConfigureVmRequest, CreateVmRequest, FsPermissionRule, FsPermissionRuleSet, FsPermissionScope,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, OwnershipScope,
    PatternPermissionRule, PatternPermissionRuleSet, PatternPermissionScope, PermissionMode,
    PermissionsPolicy, RequestPayload, ResponsePayload, RootFilesystemDescriptor,
    RootFilesystemEntry, RootFilesystemEntryKind,
};
use support::{authenticate, create_vm, open_session, request, temp_dir};

fn expect_invalid_state(payload: ResponsePayload, expected_message: &str) {
    match payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(
                rejected.message.contains(expected_message),
                "unexpected rejection: {rejected:?}"
            );
        }
        other => panic!("expected invalid_state rejection, got {other:?}"),
    }
}

fn create_vm_with_fs_permissions(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    permissions: PermissionsPolicy,
) -> String {
    let response = sidecar
        .dispatch_blocking(request(
            3,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: Default::default(),
                root_filesystem: RootFilesystemDescriptor {
                    bootstrap_entries: vec![RootFilesystemEntry {
                        path: String::from("/tmp"),
                        kind: RootFilesystemEntryKind::Directory,
                        mode: Some(0o755),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
                permissions: Some(permissions),
            }),
        ))
        .expect("create vm with fs permissions");

    match response.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("expected vm create response, got {other:?}"),
    }
}

fn mkdir_request(path: &str, recursive: bool) -> GuestFilesystemCallRequest {
    GuestFilesystemCallRequest {
        operation: GuestFilesystemOperation::Mkdir,
        path: path.to_owned(),
        destination_path: None,
        target: None,
        content: None,
        encoding: None,
        recursive,
        mode: None,
        uid: None,
        gid: None,
        atime_ms: None,
        mtime_ms: None,
        len: None,
    }
}

#[test]
fn permission_flags_reject_empty_operations_and_accept_explicit_wildcards() {
    let mut sidecar = support::new_sidecar("permission-flags-create");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);

    let rejected = sidecar
        .dispatch_blocking(request(
            3,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: Default::default(),
                root_filesystem: Default::default(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                        default: Some(PermissionMode::Deny),
                        rules: vec![FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: Vec::new(),
                            paths: vec![String::from("/**")],
                        }],
                    })),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    tool: None,
                }),
            }),
        ))
        .expect("dispatch create vm rejection");

    expect_invalid_state(
        rejected.response.payload,
        "fs.rules[0].operations must not be empty",
    );

    let accepted = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: Default::default(),
                root_filesystem: Default::default(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                        default: Some(PermissionMode::Deny),
                        rules: vec![FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("*")],
                            paths: vec![String::from("/**")],
                        }],
                    })),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    tool: None,
                }),
            }),
        ))
        .expect("dispatch create vm with wildcard");

    match accepted.response.payload {
        ResponsePayload::VmCreated(_) => {}
        other => panic!("expected vm creation with explicit wildcard, got {other:?}"),
    }
}

#[test]
fn permission_flags_reject_empty_paths_and_patterns_on_configure() {
    let mut sidecar = support::new_sidecar("permission-flags-configure");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("permission-flags-configure-cwd");
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let empty_paths = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                        default: Some(PermissionMode::Deny),
                        rules: vec![FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("read")],
                            paths: Vec::new(),
                        }],
                    })),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    tool: None,
                }),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("dispatch configure vm with empty fs paths");

    expect_invalid_state(
        empty_paths.response.payload,
        "fs.rules[0].paths must not be empty",
    );

    let empty_patterns = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: None,
                    network: Some(PatternPermissionScope::Rules(PatternPermissionRuleSet {
                        default: Some(PermissionMode::Deny),
                        rules: vec![PatternPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("*")],
                            patterns: Vec::new(),
                        }],
                    })),
                    child_process: None,
                    process: None,
                    env: None,
                    tool: None,
                }),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("dispatch configure vm with empty network patterns");

    expect_invalid_state(
        empty_patterns.response.payload,
        "network.rules[0].patterns must not be empty",
    );
}

#[test]
fn permission_flags_single_star_paths_do_not_cross_path_separators() {
    let mut sidecar = support::new_sidecar("permission-flags-single-star");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_fs_permissions(
        &mut sidecar,
        &connection_id,
        &session_id,
        PermissionsPolicy {
            fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                default: Some(PermissionMode::Deny),
                rules: vec![FsPermissionRule {
                    mode: PermissionMode::Allow,
                    operations: vec![String::from("create_dir"), String::from("stat")],
                    paths: vec![String::from("/tmp/*")],
                }],
            })),
            network: None,
            child_process: None,
            process: None,
            env: None,
            tool: None,
        },
    );

    let allow_direct_child = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(mkdir_request("/tmp/a", false)),
        ))
        .expect("create direct child directory");
    match allow_direct_child.response.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }

    let deny_nested_child = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(mkdir_request("/tmp/a/b", false)),
        ))
        .expect("attempt nested child directory create");
    match deny_nested_child.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "kernel_error");
            assert!(rejected.message.contains("EACCES"));
        }
        other => panic!("expected rejected nested mkdir response, got {other:?}"),
    }
}

#[test]
fn permission_flags_double_star_paths_allow_nested_descendants() {
    let mut sidecar = support::new_sidecar("permission-flags-double-star");
    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_fs_permissions(
        &mut sidecar,
        &connection_id,
        &session_id,
        PermissionsPolicy {
            fs: Some(FsPermissionScope::Rules(FsPermissionRuleSet {
                default: Some(PermissionMode::Deny),
                rules: vec![FsPermissionRule {
                    mode: PermissionMode::Allow,
                    operations: vec![String::from("create_dir"), String::from("stat")],
                    paths: vec![String::from("/tmp/**")],
                }],
            })),
            network: None,
            child_process: None,
            process: None,
            env: None,
            tool: None,
        },
    );

    let allow_direct_child = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(mkdir_request("/tmp/a", false)),
        ))
        .expect("create direct child directory");
    match allow_direct_child.response.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }

    let allow_nested_child = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(mkdir_request("/tmp/a/b/c", true)),
        ))
        .expect("create nested child directory");
    match allow_nested_child.response.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a/b/c");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }
}
