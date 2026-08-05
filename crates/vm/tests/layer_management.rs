mod support;

use agentos_vm::wire::{
    ConfigureVmRequest, CreateOverlayRequest, CreateVmRequest, ExportSnapshotRequest,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, ImportSnapshotRequest,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryKind, RootFilesystemLowerDescriptor, RootFilesystemMode, SealLayerRequest,
    SnapshotRootFilesystemLower,
};
use std::collections::HashMap;
use std::fs::{create_dir_all, write};
use support::{
    authenticate_wire, create_vm_wire, new_sidecar, open_session_wire, temp_dir,
    wire_permissions_allow_all, wire_request, wire_session, wire_vm,
};

const MAX_VM_LAYERS_UNDER_TEST: usize = 256;

fn root_dir(path: impl Into<String>) -> RootFilesystemEntry {
    root_entry(path, RootFilesystemEntryKind::Directory, None)
}

fn root_file(path: impl Into<String>, content: impl Into<String>) -> RootFilesystemEntry {
    root_entry(path, RootFilesystemEntryKind::File, Some(content.into()))
}

fn root_entry(
    path: impl Into<String>,
    kind: RootFilesystemEntryKind,
    content: Option<String>,
) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.into(),
        kind,
        mode: None,
        uid: None,
        gid: None,
        content,
        encoding: None,
        target: None,
        executable: false,
    }
}

#[test]
fn vm_layer_lifecycle_round_trips_snapshots_and_invalidates_sealed_ids() {
    let mut sidecar = new_sidecar("layer-lifecycle");
    let cwd = temp_dir("layer-lifecycle-cwd");

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

    let imported_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![
                    root_dir("/workspace"),
                    root_file("/workspace/note.txt", "imported"),
                ],
            }),
        ))
        .expect("import snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };

    let imported_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: imported_layer_id.clone(),
            }),
        ))
        .expect("export imported snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
        other => panic!("unexpected export snapshot response: {other:?}"),
    };
    assert!(imported_entries.iter().any(|entry| {
        entry.path == "/workspace/note.txt" && entry.content.as_deref() == Some("imported")
    }));

    let writable_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateLayerRequest,
        ))
        .expect("create writable layer")
        .response
        .payload
    {
        ResponsePayload::LayerCreatedResponse(response) => response.layer_id,
        other => panic!("unexpected create layer response: {other:?}"),
    };
    let sealed_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayerRequest(SealLayerRequest {
                layer_id: writable_layer_id.clone(),
            }),
        ))
        .expect("seal writable layer")
        .response
        .payload
    {
        ResponsePayload::LayerSealedResponse(response) => response.layer_id,
        other => panic!("unexpected seal layer response: {other:?}"),
    };
    assert_ne!(sealed_layer_id, writable_layer_id);

    let sealed_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: sealed_layer_id,
            }),
        ))
        .expect("export sealed layer")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
        other => panic!("unexpected export sealed snapshot response: {other:?}"),
    };
    assert!(sealed_entries.iter().any(|entry| entry.path == "/"));

    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            9,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: writable_layer_id.clone(),
            }),
        ))
        .expect("export sealed source layer should reject");
    match rejected.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("unexpected rejection response: {other:?}"),
    }

    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            10,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayerRequest(SealLayerRequest {
                layer_id: writable_layer_id,
            }),
        ))
        .expect("double seal should reject");
    match rejected.response.payload {
        ResponsePayload::RejectedResponse(response) => {
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

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (first_vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let (second_vm_id, _) = create_vm_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let first_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &first_vm_id),
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![
                    root_dir("/workspace"),
                    root_file("/workspace/first.txt", "first-vm"),
                ],
            }),
        ))
        .expect("import snapshot into first vm")
        .response
        .payload
    {
        ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
        other => panic!("unexpected first import response: {other:?}"),
    };
    let second_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &second_vm_id),
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![
                    root_dir("/workspace"),
                    root_file("/workspace/second.txt", "second-vm"),
                ],
            }),
        ))
        .expect("import snapshot into second vm")
        .response
        .payload
    {
        ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
        other => panic!("unexpected second import response: {other:?}"),
    };

    assert_eq!(first_layer_id, "layer-1");
    assert_eq!(second_layer_id, "layer-1");

    let first_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &first_vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: first_layer_id,
            }),
        ))
        .expect("export first vm snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
        other => panic!("unexpected first export response: {other:?}"),
    };
    let second_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &second_vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: second_layer_id,
            }),
        ))
        .expect("export second vm snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
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
fn vm_layer_store_rejects_new_layers_at_limit() {
    let mut sidecar = new_sidecar("layer-store-limit");
    let cwd = temp_dir("layer-store-limit-cwd");

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

    let mut first_layer_id = String::new();
    for index in 0..MAX_VM_LAYERS_UNDER_TEST {
        let layer_id = match sidecar
            .dispatch_wire_blocking(wire_request(
                4 + index as i64,
                wire_vm(&connection_id, &session_id, &vm_id),
                RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                    entries: vec![root_file(
                        format!("/layer-{index}.txt"),
                        format!("layer {index}"),
                    )],
                }),
            ))
            .expect("import snapshot at layer limit")
            .response
            .payload
        {
            ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
            other => panic!("unexpected import snapshot response: {other:?}"),
        };
        if index == 0 {
            first_layer_id = layer_id;
        }
    }

    for (offset, payload) in [
        (
            0,
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![root_file("/overflow-import.txt", "overflow")],
            }),
        ),
        (1, RequestPayload::CreateLayerRequest),
        (
            2,
            RequestPayload::CreateOverlayRequest(CreateOverlayRequest {
                mode: RootFilesystemMode::Ephemeral,
                upper_layer_id: None,
                lower_layer_ids: vec![first_layer_id.clone()],
            }),
        ),
        (
            3,
            RequestPayload::CreateOverlayRequest(CreateOverlayRequest {
                mode: RootFilesystemMode::Ephemeral,
                upper_layer_id: None,
                lower_layer_ids: vec![String::from("missing-layer")],
            }),
        ),
    ] {
        let rejected = sidecar
            .dispatch_wire_blocking(wire_request(
                300 + offset,
                wire_vm(&connection_id, &session_id, &vm_id),
                payload,
            ))
            .expect("dispatch layer overflow request");
        match rejected.response.payload {
            ResponsePayload::RejectedResponse(response) => {
                assert_eq!(response.code, "invalid_state");
                assert!(
                    response.message.contains("VM layer limit exceeded"),
                    "unexpected rejection: {response:?}"
                );
            }
            other => panic!("expected layer limit rejection, got {other:?}"),
        }
    }

    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            400,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: String::from("layer-257"),
            }),
        ))
        .expect("export overflow layer id should reject");
    match rejected.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("expected unknown overflow layer rejection, got {other:?}"),
    }
}

#[test]
fn create_vm_root_filesystem_composes_multiple_lowers_with_bootstrap_upper() {
    let mut sidecar = new_sidecar("vm-root-multi-layer");
    let cwd = temp_dir("vm-root-multi-layer-cwd");

    let connection_id = authenticate_wire(&mut sidecar, "conn-1");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let create = sidecar
        .dispatch_wire_blocking(wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                RootFilesystemDescriptor {
                    disable_default_base_layer: true,
                    lowers: vec![
                        RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                            SnapshotRootFilesystemLower {
                                entries: vec![
                                    root_dir("/workspace"),
                                    root_file("/workspace/shared.txt", "higher"),
                                    root_file("/workspace/higher-only.txt", "higher-only"),
                                ],
                            },
                        ),
                        RootFilesystemLowerDescriptor::SnapshotRootFilesystemLower(
                            SnapshotRootFilesystemLower {
                                entries: vec![
                                    root_dir("/workspace"),
                                    root_file("/workspace/shared.txt", "lower"),
                                    root_file("/workspace/lower-only.txt", "lower-only"),
                                ],
                            },
                        ),
                    ],
                    bootstrap_entries: vec![
                        root_dir("/workspace"),
                        root_file("/workspace/shared.txt", "upper"),
                        root_file("/workspace/upper-only.txt", "upper-only"),
                    ],
                    mode: RootFilesystemMode::Ephemeral,
                },
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create vm with multi-layer root");

    let vm_id = match create.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create vm response: {other:?}"),
    };

    for (request_id, path, expected) in [
        (4, "/workspace/shared.txt", "upper"),
        (5, "/workspace/higher-only.txt", "higher-only"),
        (6, "/workspace/lower-only.txt", "lower-only"),
        (7, "/workspace/upper-only.txt", "upper-only"),
    ] {
        let read = sidecar
            .dispatch_wire_blocking(wire_request(
                request_id,
                wire_vm(&connection_id, &session_id, &vm_id),
                RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                    operation: GuestFilesystemOperation::ReadFile,
                    path: String::from(path),
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
            .expect("read layered file");

        match read.response.payload {
            ResponsePayload::GuestFilesystemResultResponse(response) => {
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

    let configure = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: None,
                module_access_cwd: Some(module_access_cwd.to_string_lossy().into_owned()),
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
        .expect("configure vm");
    match configure.response.payload {
        ResponsePayload::VmConfiguredResponse(response) => {
            // 1 = just the module_access node_modules mount. With no packages
            // configured there are no granular `/opt/agentos` leaf mounts (the
            // projection adds a tar/bin/current mount per package, not a single
            // always-present staging mount).
            assert_eq!(response.applied_mounts, 1);
        }
        other => panic!("unexpected configure response: {other:?}"),
    }

    let module_read = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/root/node_modules/fixture-pkg/package.json"),
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
        .expect("read module access file");
    match module_read.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert!(response
                .content
                .expect("module access content")
                .contains("\"fixture-pkg\""));
        }
        other => panic!("unexpected module access response: {other:?}"),
    }

    let writable_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateLayerRequest,
        ))
        .expect("create layer")
        .response
        .payload
    {
        ResponsePayload::LayerCreatedResponse(response) => response.layer_id,
        other => panic!("unexpected create layer response: {other:?}"),
    };
    let sealed_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SealLayerRequest(SealLayerRequest {
                layer_id: writable_layer_id,
            }),
        ))
        .expect("seal layer")
        .response
        .payload
    {
        ResponsePayload::LayerSealedResponse(response) => response.layer_id,
        other => panic!("unexpected seal layer response: {other:?}"),
    };
    let sealed_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: sealed_layer_id,
            }),
        ))
        .expect("export sealed layer")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
        other => panic!("unexpected export snapshot response: {other:?}"),
    };
    assert!(sealed_entries.iter().any(|entry| entry.path == "/"));

    let lower_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            9,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![
                    root_dir("/workspace"),
                    root_file("/workspace/lower.txt", "lower"),
                    root_file("/workspace/shared.txt", "lower"),
                ],
            }),
        ))
        .expect("import lower snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };
    let upper_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            10,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ImportSnapshotRequest(ImportSnapshotRequest {
                entries: vec![
                    root_dir("/workspace"),
                    root_file("/workspace/upper.txt", "upper"),
                    root_file("/workspace/shared.txt", "upper"),
                ],
            }),
        ))
        .expect("import upper snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotImportedResponse(response) => response.layer_id,
        other => panic!("unexpected import snapshot response: {other:?}"),
    };
    let overlay_layer_id = match sidecar
        .dispatch_wire_blocking(wire_request(
            11,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::CreateOverlayRequest(CreateOverlayRequest {
                mode: RootFilesystemMode::Ephemeral,
                upper_layer_id: Some(upper_layer_id),
                lower_layer_ids: vec![lower_layer_id],
            }),
        ))
        .expect("create overlay")
        .response
        .payload
    {
        ResponsePayload::OverlayCreatedResponse(response) => response.layer_id,
        other => panic!("unexpected create overlay response: {other:?}"),
    };
    let overlay_entries = match sidecar
        .dispatch_wire_blocking(wire_request(
            12,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: overlay_layer_id.clone(),
            }),
        ))
        .expect("export overlay snapshot")
        .response
        .payload
    {
        ResponsePayload::SnapshotExportedResponse(response) => response.entries,
        other => panic!("unexpected overlay export response: {other:?}"),
    };
    assert!(overlay_entries
        .iter()
        .any(|entry| entry.path == "/workspace/lower.txt"));
    assert!(overlay_entries
        .iter()
        .any(|entry| entry.path == "/workspace/upper.txt"));
    assert!(overlay_entries
        .iter()
        .any(|entry| entry.path == "/workspace/shared.txt"
            && entry.content.as_deref() == Some("upper")));

    let (other_vm_id, _) = create_vm_wire(
        &mut sidecar,
        13,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );
    let rejected = sidecar
        .dispatch_wire_blocking(wire_request(
            14,
            wire_vm(&connection_id, &session_id, &other_vm_id),
            RequestPayload::ExportSnapshotRequest(ExportSnapshotRequest {
                layer_id: overlay_layer_id,
            }),
        ))
        .expect("export unknown layer should reject");
    match rejected.response.payload {
        ResponsePayload::RejectedResponse(response) => {
            assert_eq!(response.code, "invalid_state");
            assert!(response.message.contains("unknown layer"));
        }
        other => panic!("unexpected rejection response: {other:?}"),
    }
}
