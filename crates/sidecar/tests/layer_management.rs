mod support;

use agent_os_sidecar::protocol::{
    ConfigureVmRequest, CreateLayerRequest, CreateOverlayRequest, ExportSnapshotRequest,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, ImportSnapshotRequest,
    OwnershipScope, RequestPayload, ResponsePayload, RootFilesystemEntry,
    RootFilesystemEntryKind, RootFilesystemMode, SealLayerRequest,
};
use std::collections::BTreeMap;
use std::fs::{create_dir_all, write};
use support::{authenticate, create_vm, new_sidecar, open_session, request, temp_dir};

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
