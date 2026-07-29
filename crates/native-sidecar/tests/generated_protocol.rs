use agentos_native_sidecar::generated_protocol::v1::{
    AuthenticateRequest, ConfigureVmRequest, ConnectionOwnership, ExtEnvelope, FsPermissionScope,
    GuestFilesystemCallRequest, GuestFilesystemOperation, MountDescriptor, MountPluginDescriptor,
    OwnershipScope, PermissionMode, PermissionsPolicy, ProjectedModuleDescriptor, ProtocolFrame,
    ProtocolSchema, RequestFrame, RequestPayload, ResponseFrame, ResponsePayload,
    VmConfiguredResponse, VmOwnership, WasmPermissionTier,
};
use agentos_native_sidecar::protocol as live_protocol;
use serde_json::json;
use std::collections::HashMap;

const GENERATED_AUTH_FRAME_HEX: &str = "00166167656e746f732d6e61746976652d73696465636172080007000000000000000006636f6e6e2d31000e67656e6572617465642d7465737405746f6b656e080001000000";

#[test]
fn generated_protocol_round_trips_request_frame() {
    let frame = ProtocolFrame::RequestFrame(RequestFrame {
        schema: ProtocolSchema {
            name: live_protocol::PROTOCOL_NAME.to_string(),
            version: live_protocol::PROTOCOL_VERSION,
        },
        request_id: 7,
        ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
            connection_id: "conn-1".to_string(),
        }),
        payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
            client_name: "generated-test".to_string(),
            auth_token: "token".to_string(),
            protocol_version: live_protocol::PROTOCOL_VERSION,
            bridge_version: 1,
        }),
    });

    let encoded = serde_bare::to_vec(&frame).expect("encode generated frame");
    let decoded: ProtocolFrame = serde_bare::from_slice(&encoded).expect("decode generated frame");

    assert_eq!(decoded, frame);
}

#[test]
fn generated_protocol_matches_cross_language_auth_frame_bytes() {
    let frame = generated_auth_frame();

    let encoded = serde_bare::to_vec(&frame).expect("encode generated frame");
    assert_eq!(hex_encode(&encoded), GENERATED_AUTH_FRAME_HEX);

    let fixture = hex_decode(GENERATED_AUTH_FRAME_HEX);
    let decoded: ProtocolFrame =
        serde_bare::from_slice(&fixture).expect("decode generated auth fixture");
    assert_eq!(decoded, frame);
}

#[test]
fn live_bare_codec_matches_generated_request_bytes() {
    let codec = live_protocol::NativeFrameCodec::with_payload_codec(
        1024 * 1024,
        live_protocol::NativePayloadCodec::Bare,
    );

    let live_auth = live_protocol::ProtocolFrame::Request(live_protocol::RequestFrame::new(
        7,
        live_protocol::OwnershipScope::connection("conn-1"),
        live_protocol::RequestPayload::Authenticate(live_protocol::AuthenticateRequest {
            client_name: "generated-test".to_string(),
            auth_token: "token".to_string(),
            protocol_version: live_protocol::PROTOCOL_VERSION,
            bridge_version: 1,
        }),
    ));
    let live_auth_payload = live_frame_payload(&codec.encode(&live_auth).expect("encode auth"));
    let generated_auth_payload =
        serde_bare::to_vec(&generated_auth_frame()).expect("encode generated auth");
    assert_eq!(live_auth_payload, generated_auth_payload);

    let live_configure = live_protocol::ProtocolFrame::Request(live_protocol::RequestFrame::new(
        9,
        live_protocol::OwnershipScope::vm("conn-1", "session-1", "vm-1"),
        live_protocol::RequestPayload::ConfigureVm(live_protocol::ConfigureVmRequest {
            mounts: vec![live_protocol::MountDescriptor {
                guest_path: "/node_modules".to_string(),
                guest_source: "host_dir".to_string(),
                guest_fstype: "host_dir".to_string(),
                read_only: true,
                plugin: live_protocol::MountPluginDescriptor {
                    id: "host_dir".to_string(),
                    config: json!({
                        "hostPath": "/tmp/deps",
                        "readOnly": true,
                    })
                    .to_string(),
                },
            }],
            software: Vec::new(),
            permissions: Some(live_protocol::PermissionsPolicy {
                fs: Some(live_protocol::FsPermissionScope::PermissionMode(
                    live_protocol::PermissionMode::Allow,
                )),
                network: None,
                child_process: None,
                process: None,
                env: None,
                binding: None,
            }),
            module_access_cwd: Some("/workspace".to_string()),
            instructions: vec!["keep it generic".to_string()],
            projected_modules: vec![live_protocol::ProjectedModuleDescriptor {
                package_name: "workspace".to_string(),
                entrypoint: "/workspace/index.js".to_string(),
            }],
            command_permissions: std::collections::HashMap::from([(
                "cat".to_string(),
                live_protocol::WasmPermissionTier::ReadOnly,
            )]),
            loopback_exempt_ports: vec![3000],
            packages: Vec::new(),
            packages_mount_at: String::new(),
            bootstrap_commands: Vec::new(),
            binding_shim_commands: Vec::new(),
        }),
    ));
    let live_configure_payload =
        live_frame_payload(&codec.encode(&live_configure).expect("encode configure"));
    let generated_configure_payload =
        serde_bare::to_vec(&generated_configure_frame()).expect("encode generated configure");
    assert_eq!(live_configure_payload, generated_configure_payload);

    let live_ext = live_protocol::ProtocolFrame::Request(live_protocol::RequestFrame::new(
        11,
        live_protocol::OwnershipScope::connection("conn-1"),
        live_protocol::RequestPayload::Ext(live_protocol::ExtEnvelope {
            namespace: "dev.rivet.agentos.test".to_string(),
            payload: b"extension-bytes".to_vec(),
        }),
    ));
    let live_ext_payload = live_frame_payload(&codec.encode(&live_ext).expect("encode ext"));
    let generated_ext_payload =
        serde_bare::to_vec(&generated_ext_frame()).expect("encode generated ext");
    assert_eq!(live_ext_payload, generated_ext_payload);
}

#[test]
fn live_bare_codec_decodes_generated_response_bytes() {
    let codec = live_protocol::NativeFrameCodec::with_payload_codec(
        1024 * 1024,
        live_protocol::NativePayloadCodec::Bare,
    );
    let generated = ProtocolFrame::ResponseFrame(ResponseFrame {
        schema: protocol_schema(),
        request_id: 9,
        ownership: generated_vm_ownership(),
        payload: ResponsePayload::VmConfiguredResponse(VmConfiguredResponse {
            applied_mounts: 2,
            applied_software: 0,
            projected_commands: Vec::new(),
            agents: Vec::new(),
        }),
    });
    let payload = serde_bare::to_vec(&generated).expect("encode generated response");
    let decoded = codec
        .decode(&framed_payload(&payload))
        .expect("decode generated response with live codec");

    assert_eq!(
        decoded,
        live_protocol::ProtocolFrame::Response(live_protocol::ResponseFrame::new(
            9,
            live_protocol::OwnershipScope::vm("conn-1", "session-1", "vm-1"),
            live_protocol::ResponsePayload::VmConfigured(live_protocol::VmConfiguredResponse {
                applied_mounts: 2,
                applied_software: 0,
                projected_commands: Vec::new(),
                agents: Vec::new(),
            }),
        )),
    );
}

#[test]
fn generated_protocol_preserves_json_utf8_strings() {
    let descriptor = MountPluginDescriptor {
        id: "chunked_s3".to_string(),
        config: r#"{"bucket":"demo","prefix":"workspace"}"#.to_string(),
    };

    let encoded = serde_bare::to_vec(&descriptor).expect("encode generated descriptor");
    let decoded: MountPluginDescriptor =
        serde_bare::from_slice(&encoded).expect("decode generated descriptor");

    assert_eq!(decoded, descriptor);
}

#[test]
fn generated_protocol_preserves_guest_filesystem_call_offsets() {
    let request = GuestFilesystemCallRequest {
        operation: GuestFilesystemOperation::Pread,
        path: "/workspace/data.bin".to_string(),
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
        len: Some(12),
        offset: Some(34),
    };

    let encoded = serde_bare::to_vec(&request).expect("encode generated filesystem call");
    let decoded: GuestFilesystemCallRequest =
        serde_bare::from_slice(&encoded).expect("decode generated filesystem call");

    assert_eq!(decoded, request);
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn hex_decode(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "hex length must be even");
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]))
        .collect()
}

fn generated_auth_frame() -> ProtocolFrame {
    ProtocolFrame::RequestFrame(RequestFrame {
        schema: protocol_schema(),
        request_id: 7,
        ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
            connection_id: "conn-1".to_string(),
        }),
        payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
            client_name: "generated-test".to_string(),
            auth_token: "token".to_string(),
            protocol_version: live_protocol::PROTOCOL_VERSION,
            bridge_version: 1,
        }),
    })
}

fn generated_configure_frame() -> ProtocolFrame {
    ProtocolFrame::RequestFrame(RequestFrame {
        schema: protocol_schema(),
        request_id: 9,
        ownership: generated_vm_ownership(),
        payload: RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
            mounts: vec![MountDescriptor {
                guest_path: "/node_modules".to_string(),
                guest_source: "host_dir".to_string(),
                guest_fstype: "host_dir".to_string(),
                read_only: true,
                plugin: MountPluginDescriptor {
                    id: "host_dir".to_string(),
                    config: r#"{"hostPath":"/tmp/deps","readOnly":true}"#.to_string(),
                },
            }],
            software: Vec::new(),
            permissions: Some(PermissionsPolicy {
                fs: Some(FsPermissionScope::PermissionMode(PermissionMode::Allow)),
                network: None,
                child_process: None,
                process: None,
                env: None,
                binding: None,
            }),
            module_access_cwd: Some("/workspace".to_string()),
            instructions: vec!["keep it generic".to_string()],
            projected_modules: vec![ProjectedModuleDescriptor {
                package_name: "workspace".to_string(),
                entrypoint: "/workspace/index.js".to_string(),
            }],
            command_permissions: HashMap::from([("cat".to_string(), WasmPermissionTier::ReadOnly)]),
            loopback_exempt_ports: vec![3000],
            packages: Vec::new(),
            packages_mount_at: String::new(),
            bootstrap_commands: Vec::new(),
            binding_shim_commands: Vec::new(),
        }),
    })
}

fn protocol_schema() -> ProtocolSchema {
    ProtocolSchema {
        name: live_protocol::PROTOCOL_NAME.to_string(),
        version: live_protocol::PROTOCOL_VERSION,
    }
}

fn generated_ext_frame() -> ProtocolFrame {
    ProtocolFrame::RequestFrame(RequestFrame {
        schema: protocol_schema(),
        request_id: 11,
        ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
            connection_id: "conn-1".to_string(),
        }),
        payload: RequestPayload::ExtEnvelope(ExtEnvelope {
            namespace: "dev.rivet.agentos.test".to_string(),
            payload: b"extension-bytes".to_vec(),
        }),
    })
}

fn generated_vm_ownership() -> OwnershipScope {
    OwnershipScope::VmOwnership(VmOwnership {
        connection_id: "conn-1".to_string(),
        session_id: "session-1".to_string(),
        vm_id: "vm-1".to_string(),
    })
}

fn live_frame_payload(frame: &[u8]) -> Vec<u8> {
    frame[4..].to_vec()
}

fn framed_payload(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => panic!("invalid hex byte {byte}"),
    }
}
