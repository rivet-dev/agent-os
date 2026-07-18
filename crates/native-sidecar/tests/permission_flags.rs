mod support;

use agentos_native_sidecar::wire::{
    ConfigureVmRequest, CreateVmRequest, FsPermissionRule, FsPermissionRuleSet, FsPermissionScope,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, PatternPermissionRule,
    PatternPermissionRuleSet, PatternPermissionScope, PermissionMode, PermissionsPolicy,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryKind, RootFilesystemMode,
};
use std::collections::HashMap;
use support::{
    authenticate_wire, create_vm_wire, open_session_wire, temp_dir, wire_request, wire_session,
    wire_vm,
};

fn expect_invalid_state(payload: ResponsePayload, expected_message: &str) {
    match payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(
                rejected.message.contains(expected_message),
                "unexpected rejection: {rejected:?}"
            );
        }
        other => panic!("expected invalid_state rejection, got {other:?}"),
    }
}

fn root_dir(path: &str, mode: u32) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.to_owned(),
        kind: RootFilesystemEntryKind::Directory,
        mode: Some(mode),
        uid: None,
        gid: None,
        content: None,
        encoding: None,
        target: None,
        executable: false,
    }
}

fn create_vm_with_fs_permissions(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    permissions: PermissionsPolicy,
) -> String {
    let response = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(connection_id, session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::new(),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: vec![root_dir("/tmp", 0o755)],
                },
                Some(permissions),
            )),
        ))
        .expect("create vm with fs permissions");

    match response.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
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
        max_depth: None,
        mode: None,
        uid: None,
        gid: None,
        atime_ms: None,
        mtime_ms: None,
        len: None,
        offset: None,
    }
}

#[test]
fn permission_flags_reject_empty_operations_and_accept_explicit_wildcards() {
    let mut sidecar = support::new_sidecar("permission-flags-create");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);

    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::new(),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::FsPermissionRuleSet(
                        FsPermissionRuleSet {
                            default: Some(PermissionMode::Deny),
                            rules: vec![FsPermissionRule {
                                mode: PermissionMode::Allow,
                                operations: Vec::new(),
                                paths: vec![String::from("/**")],
                            }],
                        },
                    )),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    binding: None,
                }),
            )),
        ))
        .expect("dispatch create vm rejection");

    expect_invalid_state(
        rejected.response.payload,
        "fs.rules[0].operations must not be empty",
    );

    let accepted = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::new(),
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::FsPermissionRuleSet(
                        FsPermissionRuleSet {
                            default: Some(PermissionMode::Deny),
                            rules: vec![FsPermissionRule {
                                mode: PermissionMode::Allow,
                                operations: vec![String::from("*")],
                                paths: vec![String::from("/**")],
                            }],
                        },
                    )),
                    network: None,
                    child_process: None,
                    process: None,
                    env: None,
                    binding: None,
                }),
            )),
        ))
        .expect("dispatch create vm with wildcard");

    match accepted.response.payload {
        ResponsePayload::VmCreatedResponse(_) => {}
        other => panic!("expected vm creation with explicit wildcard, got {other:?}"),
    }
}

#[test]
fn permission_flags_reject_empty_paths_and_patterns_on_configure() {
    let mut sidecar = support::new_sidecar("permission-flags-configure");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let cwd = temp_dir("permission-flags-configure-cwd");
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let empty_paths = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: Some(FsPermissionScope::FsPermissionRuleSet(
                        FsPermissionRuleSet {
                            default: Some(PermissionMode::Deny),
                            rules: vec![FsPermissionRule {
                                mode: PermissionMode::Allow,
                                operations: vec![String::from("read")],
                                paths: Vec::new(),
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
                command_permissions: Default::default(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("dispatch configure vm with empty fs paths");

    expect_invalid_state(
        empty_paths.response.payload,
        "fs.rules[0].paths must not be empty",
    );

    let empty_patterns = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: None,
                    network: Some(PatternPermissionScope::PatternPermissionRuleSet(
                        PatternPermissionRuleSet {
                            default: Some(PermissionMode::Deny),
                            rules: vec![PatternPermissionRule {
                                mode: PermissionMode::Allow,
                                operations: vec![String::from("*")],
                                patterns: Vec::new(),
                            }],
                        },
                    )),
                    child_process: None,
                    process: None,
                    env: None,
                    binding: None,
                }),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("dispatch configure vm with empty network patterns");

    expect_invalid_state(
        empty_patterns.response.payload,
        "network.rules[0].patterns must not be empty",
    );

    let empty_pattern_operations = sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy {
                    fs: None,
                    network: Some(PatternPermissionScope::PatternPermissionRuleSet(
                        PatternPermissionRuleSet {
                            default: Some(PermissionMode::Deny),
                            rules: vec![PatternPermissionRule {
                                mode: PermissionMode::Allow,
                                operations: Vec::new(),
                                patterns: vec![String::from("**")],
                            }],
                        },
                    )),
                    child_process: None,
                    process: None,
                    env: None,
                    binding: None,
                }),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("dispatch configure vm with empty network operations");

    expect_invalid_state(
        empty_pattern_operations.response.payload,
        "network.rules[0].operations must not be empty",
    );
}

#[test]
fn permission_flags_single_star_paths_do_not_cross_path_separators() {
    let mut sidecar = support::new_sidecar("permission-flags-single-star");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_fs_permissions(
        &mut sidecar,
        &connection_id,
        &session_id,
        PermissionsPolicy {
            fs: Some(FsPermissionScope::FsPermissionRuleSet(
                FsPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![
                        FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("read")],
                            paths: vec![String::from("/tmp")],
                        },
                        FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![
                                String::from("create_dir"),
                                String::from("read"),
                                String::from("stat"),
                            ],
                            paths: vec![String::from("/tmp/*")],
                        },
                    ],
                },
            )),
            network: None,
            child_process: None,
            process: None,
            env: None,
            binding: None,
        },
    );

    let allow_direct_child = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(mkdir_request("/tmp/a", false)),
        ))
        .expect("create direct child directory");
    match allow_direct_child.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }

    let deny_nested_child = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(mkdir_request("/tmp/a/b", false)),
        ))
        .expect("attempt nested child directory create");
    match deny_nested_child.response.payload {
        ResponsePayload::RejectedResponse(rejected) => {
            assert_eq!(rejected.code, "EACCES");
            assert!(rejected.message.contains("EACCES"));
        }
        other => panic!("expected rejected nested mkdir response, got {other:?}"),
    }
}

#[test]
fn permission_flags_double_star_paths_allow_nested_descendants() {
    let mut sidecar = support::new_sidecar("permission-flags-double-star");
    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_fs_permissions(
        &mut sidecar,
        &connection_id,
        &session_id,
        PermissionsPolicy {
            fs: Some(FsPermissionScope::FsPermissionRuleSet(
                FsPermissionRuleSet {
                    default: Some(PermissionMode::Deny),
                    rules: vec![
                        FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![String::from("read")],
                            paths: vec![String::from("/tmp")],
                        },
                        FsPermissionRule {
                            mode: PermissionMode::Allow,
                            operations: vec![
                                String::from("create_dir"),
                                String::from("read"),
                                String::from("stat"),
                            ],
                            paths: vec![String::from("/tmp/**")],
                        },
                    ],
                },
            )),
            network: None,
            child_process: None,
            process: None,
            env: None,
            binding: None,
        },
    );

    let allow_direct_child = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(mkdir_request("/tmp/a", false)),
        ))
        .expect("create direct child directory");
    match allow_direct_child.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }

    let allow_nested_child = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(mkdir_request("/tmp/a/b/c", true)),
        ))
        .expect("create nested child directory");
    match allow_nested_child.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
            assert_eq!(response.path, "/tmp/a/b/c");
        }
        other => panic!("expected guest filesystem mkdir response, got {other:?}"),
    }
}
