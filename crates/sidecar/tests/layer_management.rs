mod support;

use agent_os_sidecar::protocol::{
    ConfigureVmRequest, CreateLayerRequest, CreateOverlayRequest, CreateVmRequest,
    ExportSnapshotRequest, GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind,
    ImportSnapshotRequest, OwnershipScope, PermissionsPolicy, RequestPayload, ResponsePayload,
    RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryKind,
    RootFilesystemLowerDescriptor, RootFilesystemMode, SealLayerRequest,
};
use std::collections::BTreeMap;
use std::fs::{create_dir_all, write};
use support::{authenticate, create_vm, new_sidecar, open_session, request, temp_dir};

#[test]
fn vm_layer_lifecycle_round_trips_snapshots_and_invalidates_sealed_ids() {
    let mut sidecar = new_sidecar("layer-lifecycle");
    let cwd = temp_dir("layer-lifecycle-cwd");

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

    let imported_layer_id = match sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshot(ImportSnapshotRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/note.txt"),
                        kind: RootFilesystemEntryKind::File,
                        content: Some(String::from("imported")),
                        executable: false,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("import snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImported(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };

    let imported_entries = match sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: imported_layer_id.clone(),
            }),
        ))
        .expect("export imported snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected export snapshot response: {other:?}"),
    };
    assert!(imported_entries.iter().any(|entry| {
        entry.path == "/workspace/note.txt" && entry.content.as_deref() == Some("imported")
    }));

    let writable_layer_id = match sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateLayer(CreateLayerRequest::default()),
        ))
        .expect("create writable layer")
        .response
        .payload
    {
        ResponsePayload::LayerCreated(response) => response.layer_id,
        other => panic!("unexpected create layer response: {other:?}"),
    };
    let sealed_layer_id = match sidecar
        .dispatch_blocking(request(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayer(SealLayerRequest {
                layer_id: writable_layer_id.clone(),
            }),
        ))
        .expect("seal writable layer")
        .response
        .payload
    {
        ResponsePayload::LayerSealed(response) => response.layer_id,
        other => panic!("unexpected seal layer response: {other:?}"),
    };
    assert_ne!(sealed_layer_id, writable_layer_id);

    let sealed_entries = match sidecar
        .dispatch_blocking(request(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: sealed_layer_id,
            }),
        ))
        .expect("export sealed layer")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected export sealed snapshot response: {other:?}"),
    };
    assert!(sealed_entries.iter().any(|entry| entry.path == "/"));

    let rejected = sidecar
        .dispatch_blocking(request(
            9,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: writable_layer_id.clone(),
            }),
        ))
        .expect("export sealed source layer should reject");
    match rejected.response.payload {
        ResponsePayload::Rejected(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("unexpected rejection response: {other:?}"),
    }

    let rejected = sidecar
        .dispatch_blocking(request(
            10,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayer(SealLayerRequest {
                layer_id: writable_layer_id,
            }),
        ))
        .expect("double seal should reject");
    match rejected.response.payload {
        ResponsePayload::Rejected(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("unexpected rejection response: {other:?}"),
    }
}

#[test]
fn vm_layer_ids_are_reused_per_vm_without_cross_vm_leakage() {
    let mut sidecar = new_sidecar("layer-store-isolation");
    let cwd = temp_dir("layer-store-isolation-cwd");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (first_vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (second_vm_id, _) = create_vm(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let first_layer_id = match sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &first_vm_id),
            RequestPayload::ImportSnapshot(ImportSnapshotRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/first.txt"),
                        kind: RootFilesystemEntryKind::File,
                        content: Some(String::from("first-vm")),
                        executable: false,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("import snapshot into first vm")
        .response
        .payload
    {
        ResponsePayload::SnapshotImported(response) => response.layer_id,
        other => panic!("unexpected first import response: {other:?}"),
    };
    let second_layer_id = match sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &second_vm_id),
            RequestPayload::ImportSnapshot(ImportSnapshotRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/second.txt"),
                        kind: RootFilesystemEntryKind::File,
                        content: Some(String::from("second-vm")),
                        executable: false,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("import snapshot into second vm")
        .response
        .payload
    {
        ResponsePayload::SnapshotImported(response) => response.layer_id,
        other => panic!("unexpected second import response: {other:?}"),
    };

    assert_eq!(first_layer_id, "layer-1");
    assert_eq!(second_layer_id, "layer-1");

    let first_entries = match sidecar
        .dispatch_blocking(request(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &first_vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: first_layer_id,
            }),
        ))
        .expect("export first vm snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected first export response: {other:?}"),
    };
    let second_entries = match sidecar
        .dispatch_blocking(request(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &second_vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: second_layer_id,
            }),
        ))
        .expect("export second vm snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected second export response: {other:?}"),
    };

    assert!(first_entries.iter().any(|entry| {
        entry.path == "/workspace/first.txt" && entry.content.as_deref() == Some("first-vm")
    }));
    assert!(!first_entries
        .iter()
        .any(|entry| entry.path == "/workspace/second.txt"));
    assert!(second_entries.iter().any(|entry| {
        entry.path == "/workspace/second.txt" && entry.content.as_deref() == Some("second-vm")
    }));
    assert!(!second_entries
        .iter()
        .any(|entry| entry.path == "/workspace/first.txt"));
}

#[test]
fn create_vm_root_filesystem_composes_multiple_lowers_with_bootstrap_upper() {
    let mut sidecar = new_sidecar("vm-root-multi-layer");
    let cwd = temp_dir("vm-root-multi-layer-cwd");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let create = sidecar
        .dispatch_blocking(request(
            3,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem: RootFilesystemDescriptor {
                    disable_default_base_layer: true,
                    lowers: vec![
                        RootFilesystemLowerDescriptor::Snapshot {
                            entries: vec![
                                RootFilesystemEntry {
                                    path: String::from("/workspace"),
                                    kind: RootFilesystemEntryKind::Directory,
                                    executable: false,
                                    ..Default::default()
                                },
                                RootFilesystemEntry {
                                    path: String::from("/workspace/shared.txt"),
                                    kind: RootFilesystemEntryKind::File,
                                    content: Some(String::from("higher")),
                                    executable: false,
                                    ..Default::default()
                                },
                                RootFilesystemEntry {
                                    path: String::from("/workspace/higher-only.txt"),
                                    kind: RootFilesystemEntryKind::File,
                                    content: Some(String::from("higher-only")),
                                    executable: false,
                                    ..Default::default()
                                },
                            ],
                        },
                        RootFilesystemLowerDescriptor::Snapshot {
                            entries: vec![
                                RootFilesystemEntry {
                                    path: String::from("/workspace"),
                                    kind: RootFilesystemEntryKind::Directory,
                                    executable: false,
                                    ..Default::default()
                                },
                                RootFilesystemEntry {
                                    path: String::from("/workspace/shared.txt"),
                                    kind: RootFilesystemEntryKind::File,
                                    content: Some(String::from("lower")),
                                    executable: false,
                                    ..Default::default()
                                },
                                RootFilesystemEntry {
                                    path: String::from("/workspace/lower-only.txt"),
                                    kind: RootFilesystemEntryKind::File,
                                    content: Some(String::from("lower-only")),
                                    executable: false,
                                    ..Default::default()
                                },
                            ],
                        },
                    ],
                    bootstrap_entries: vec![
                        RootFilesystemEntry {
                            path: String::from("/workspace"),
                            kind: RootFilesystemEntryKind::Directory,
                            executable: false,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/workspace/shared.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("upper")),
                            executable: false,
                            ..Default::default()
                        },
                        RootFilesystemEntry {
                            path: String::from("/workspace/upper-only.txt"),
                            kind: RootFilesystemEntryKind::File,
                            content: Some(String::from("upper-only")),
                            executable: false,
                            ..Default::default()
                        },
                    ],
                    ..RootFilesystemDescriptor::default()
                },
                permissions: Some(PermissionsPolicy::allow_all()),
            }),
        ))
        .expect("create vm with multi-layer root");

    let vm_id = match create.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected create vm response: {other:?}"),
    };

    for (request_id, path, expected) in [
        (4, "/workspace/shared.txt", "upper"),
        (5, "/workspace/higher-only.txt", "higher-only"),
        (6, "/workspace/lower-only.txt", "lower-only"),
        (7, "/workspace/upper-only.txt", "upper-only"),
    ] {
        let read = sidecar
            .dispatch_blocking(request(
                request_id,
                OwnershipScope::vm(&connection_id, &session_id, &vm_id),
                RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::ReadFile,
                    path: String::from(path),
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
            .expect("read layered file");

        match read.response.payload {
            ResponsePayload::GuestFilesystemResult(response) => {
                assert_eq!(response.content.as_deref(), Some(expected));
            }
            other => panic!("unexpected guest filesystem response: {other:?}"),
        }
    }
}

#[test]
fn vm_layer_rpcs_and_module_access_mounts_are_scoped_per_vm() {
    let mut sidecar = new_sidecar("layer-management");
    let cwd = temp_dir("layer-management-cwd");
    let module_access_cwd = temp_dir("layer-management-module-access");
    let package_root = module_access_cwd.join("node_modules/fixture-pkg");
    create_dir_all(&package_root).expect("create module access package root");
    write(
        package_root.join("package.json"),
        r#"{"name":"fixture-pkg","version":"1.0.0"}"#,
    )
    .expect("write module access package json");

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

    let configure = sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: None,
                module_access_cwd: Some(module_access_cwd.to_string_lossy().into_owned()),
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: BTreeMap::new(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("configure vm");
    match configure.response.payload {
        ResponsePayload::VmConfigured(response) => {
            assert_eq!(response.applied_mounts, 1);
        }
        other => panic!("unexpected configure response: {other:?}"),
    }

    let module_read = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/root/node_modules/fixture-pkg/package.json"),
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
        .expect("read module access file");
    match module_read.response.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert!(response
                .content
                .expect("module access content")
                .contains("\"fixture-pkg\""));
        }
        other => panic!("unexpected module access response: {other:?}"),
    }

    let writable_layer_id = match sidecar
        .dispatch_blocking(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateLayer(CreateLayerRequest::default()),
        ))
        .expect("create layer")
        .response
        .payload
    {
        ResponsePayload::LayerCreated(response) => response.layer_id,
        other => panic!("unexpected create layer response: {other:?}"),
    };
    let sealed_layer_id = match sidecar
        .dispatch_blocking(request(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayer(SealLayerRequest {
                layer_id: writable_layer_id,
            }),
        ))
        .expect("seal layer")
        .response
        .payload
    {
        ResponsePayload::LayerSealed(response) => response.layer_id,
        other => panic!("unexpected seal layer response: {other:?}"),
    };
    let sealed_entries = match sidecar
        .dispatch_blocking(request(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: sealed_layer_id,
            }),
        ))
        .expect("export sealed layer")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected export snapshot response: {other:?}"),
    };
    assert!(sealed_entries.iter().any(|entry| entry.path == "/"));

    let lower_layer_id = match sidecar
        .dispatch_blocking(request(
            9,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshot(ImportSnapshotRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/lower.txt"),
                        kind: RootFilesystemEntryKind::File,
                        content: Some(String::from("lower")),
                        executable: false,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("import lower snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImported(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };
    let upper_layer_id = match sidecar
        .dispatch_blocking(request(
            10,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshot(ImportSnapshotRequest {
                entries: vec![
                    RootFilesystemEntry {
                        path: String::from("/workspace"),
                        kind: RootFilesystemEntryKind::Directory,
                        executable: false,
                        ..Default::default()
                    },
                    RootFilesystemEntry {
                        path: String::from("/workspace/upper.txt"),
                        kind: RootFilesystemEntryKind::File,
                        content: Some(String::from("upper")),
                        executable: false,
                        ..Default::default()
                    },
                ],
            }),
        ))
        .expect("import upper snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImported(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };
    let overlay_layer_id = match sidecar
        .dispatch_blocking(request(
            11,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateOverlay(CreateOverlayRequest {
                mode: RootFilesystemMode::Ephemeral,
                upper_layer_id: Some(upper_layer_id),
                lower_layer_ids: vec![lower_layer_id],
            }),
        ))
        .expect("create overlay")
        .response
        .payload
    {
        ResponsePayload::OverlayCreated(response) => response.layer_id,
        other => panic!("unexpected create overlay response: {other:?}"),
    };
    let overlay_entries = match sidecar
        .dispatch_blocking(request(
            12,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: overlay_layer_id.clone(),
            }),
        ))
        .expect("export overlay snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExported(response) => response.entries,
        other => panic!("unexpected overlay export response: {other:?}"),
    };
    assert!(overlay_entries
        .iter()
        .any(|entry| entry.path == "/workspace/lower.txt"));
    assert!(overlay_entries
        .iter()
        .any(|entry| entry.path == "/workspace/upper.txt"));

    let (other_vm_id, _) = create_vm(
        &mut sidecar,
        13,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let rejected = sidecar
        .dispatch_blocking(request(
            14,
            OwnershipScope::vm(&connection_id, &session_id, &other_vm_id),
            RequestPayload::ExportSnapshot(ExportSnapshotRequest {
                layer_id: overlay_layer_id,
            }),
        ))
        .expect("export unknown layer should reject");
    match rejected.response.payload {
        ResponsePayload::Rejected(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("unexpected rejection response: {other:?}"),
    }
}
