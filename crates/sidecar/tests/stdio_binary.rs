mod support;

use agent_os_sidecar::protocol::{
    AuthenticateRequest, ConfigureVmRequest, CreateVmRequest, EventPayload, ExecuteRequest,
    GuestFilesystemCallRequest, GuestFilesystemOperation, GuestRuntimeKind, MountDescriptor,
    MountPluginDescriptor, NativeFrameCodec, OpenSessionRequest, OwnershipScope, ProtocolFrame,
    RequestFrame, RequestPayload, ResponseFrame, ResponsePayload, SidecarPlacement,
    SnapshotRootFilesystemRequest, StreamChannel,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use support::temp_dir;

fn send_request(stdin: &mut ChildStdin, codec: &NativeFrameCodec, request: RequestFrame) {
    let encoded = codec
        .encode(&ProtocolFrame::Request(request))
        .expect("encode request");
    stdin.write_all(&encoded).expect("write request");
    stdin.flush().expect("flush request");
}

fn read_frame(stdout: &mut ChildStdout, codec: &NativeFrameCodec) -> ProtocolFrame {
    let mut prefix = [0u8; 4];
    stdout.read_exact(&mut prefix).expect("read length prefix");
    let declared = u32::from_be_bytes(prefix) as usize;
    let mut bytes = Vec::with_capacity(4 + declared);
    bytes.extend_from_slice(&prefix);
    bytes.resize(4 + declared, 0);
    stdout
        .read_exact(&mut bytes[4..])
        .expect("read framed payload");
    codec.decode(&bytes).expect("decode frame")
}

fn recv_response(
    stdout: &mut ChildStdout,
    codec: &NativeFrameCodec,
    request_id: u64,
    events: &mut Vec<EventPayload>,
) -> ResponseFrame {
    loop {
        match read_frame(stdout, codec) {
            ProtocolFrame::Response(response) if response.request_id == request_id => {
                return response;
            }
            ProtocolFrame::Event(event) => events.push(event.payload),
            other => panic!("unexpected frame while waiting for response {request_id}: {other:?}"),
        }
    }
}

fn collect_process_events(
    stdout: &mut ChildStdout,
    codec: &NativeFrameCodec,
    process_id: &str,
) -> (String, String, i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout_text = String::new();
    let mut stderr_text = String::new();

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process events"
        );
        match read_frame(stdout, codec) {
            ProtocolFrame::Event(event) => match event.payload {
                EventPayload::ProcessOutput(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => stdout_text.push_str(&output.chunk),
                        StreamChannel::Stderr => stderr_text.push_str(&output.chunk),
                    }
                }
                EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                    return (stdout_text, stderr_text, exited.exit_code);
                }
                _ => {}
            },
            other => panic!("unexpected frame while waiting for process events: {other:?}"),
        }
    }
}

fn collect_vm_lifecycle_states(
    stdout: &mut ChildStdout,
    codec: &NativeFrameCodec,
    count: usize,
) -> Vec<agent_os_sidecar::protocol::VmLifecycleState> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut states = Vec::new();

    while states.len() < count {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for VM lifecycle events"
        );
        match read_frame(stdout, codec) {
            ProtocolFrame::Event(event) => {
                if let EventPayload::VmLifecycle(lifecycle) = event.payload {
                    states.push(lifecycle.state);
                }
            }
            other => panic!("unexpected frame while waiting for lifecycle events: {other:?}"),
        }
    }

    states
}

fn spawn_sidecar_binary() -> (Child, ChildStdin, ChildStdout) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-os-sidecar"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native sidecar binary");
    let stdin = child.stdin.take().expect("capture sidecar stdin");
    let stdout = child.stdout.take().expect("capture sidecar stdout");
    (child, stdin, stdout)
}

fn write_script(root: &Path) {
    fs::write(root.join("entry.mjs"), "console.log('stdio-binary-ok');\n")
        .expect("write test entrypoint");
}

#[test]
fn native_sidecar_binary_runs_the_framed_protocol_over_stdio() {
    let temp = temp_dir("stdio-binary");
    write_script(&temp);

    let (mut child, mut stdin, mut stdout) = spawn_sidecar_binary();
    let codec = NativeFrameCodec::default();
    let mut buffered_events = Vec::new();

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            1,
            OwnershipScope::connection("client-hint"),
            RequestPayload::Authenticate(AuthenticateRequest {
                client_name: String::from("stdio-test"),
                auth_token: String::from("stdio-test-token"),
            }),
        ),
    );
    let authenticated = recv_response(&mut stdout, &codec, 1, &mut buffered_events);
    let connection_id = match authenticated.payload {
        ResponsePayload::Authenticated(response) => response.connection_id,
        other => panic!("unexpected authenticate response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            2,
            OwnershipScope::connection(&connection_id),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: SidecarPlacement::Shared { pool: None },
                metadata: BTreeMap::new(),
            }),
        ),
    );
    let session_opened = recv_response(&mut stdout, &codec, 2, &mut buffered_events);
    let session_id = match session_opened.payload {
        ResponsePayload::SessionOpened(response) => response.session_id,
        other => panic!("unexpected open-session response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            3,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    temp.to_string_lossy().into_owned(),
                )]),
                root_filesystem: Default::default(),
                permissions: Vec::new(),
            }),
        ),
    );
    let created = recv_response(&mut stdout, &codec, 3, &mut buffered_events);
    let vm_id = match created.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected create-vm response: {other:?}"),
    };
    let lifecycle_states = collect_vm_lifecycle_states(&mut stdout, &codec, 2);
    assert_eq!(
        lifecycle_states,
        vec![
            agent_os_sidecar::protocol::VmLifecycleState::Creating,
            agent_os_sidecar::protocol::VmLifecycleState::Ready,
        ]
    );

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Mkdir,
                path: String::from("/workspace"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: true,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ),
    );
    let mkdir = recv_response(&mut stdout, &codec, 4, &mut buffered_events);
    match mkdir.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.path, "/workspace");
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
        }
        other => panic!("unexpected mkdir response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/note.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("stdio-sidecar-fs")),
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ),
    );
    let write = recv_response(&mut stdout, &codec, 5, &mut buffered_events);
    match write.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.path, "/workspace/note.txt");
            assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
        }
        other => panic!("unexpected write response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/note.txt"),
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
        ),
    );
    let read = recv_response(&mut stdout, &codec, 6, &mut buffered_events);
    match read.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.content.as_deref(), Some("stdio-sidecar-fs"));
        }
        other => panic!("unexpected read response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Symlink,
                path: String::from("/workspace/link.txt"),
                destination_path: None,
                target: Some(String::from("/workspace/note.txt")),
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
        ),
    );
    let symlink = recv_response(&mut stdout, &codec, 7, &mut buffered_events);
    match symlink.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Symlink);
            assert_eq!(response.target.as_deref(), Some("/workspace/note.txt"));
        }
        other => panic!("unexpected symlink response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            8,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Realpath,
                path: String::from("/workspace/link.txt"),
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
        ),
    );
    let realpath = recv_response(&mut stdout, &codec, 8, &mut buffered_events);
    match realpath.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Realpath);
            assert_eq!(response.target.as_deref(), Some("/workspace/note.txt"));
        }
        other => panic!("unexpected realpath response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            9,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Link,
                path: String::from("/workspace/note.txt"),
                destination_path: Some(String::from("/workspace/hard.txt")),
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
        ),
    );
    let link = recv_response(&mut stdout, &codec, 9, &mut buffered_events);
    match link.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Link);
            assert_eq!(response.target.as_deref(), Some("/workspace/hard.txt"));
        }
        other => panic!("unexpected link response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            10,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Truncate,
                path: String::from("/workspace/hard.txt"),
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
                len: Some(5),
            }),
        ),
    );
    let truncate = recv_response(&mut stdout, &codec, 10, &mut buffered_events);
    match truncate.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Truncate);
            assert_eq!(response.path, "/workspace/hard.txt");
        }
        other => panic!("unexpected truncate response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            11,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Utimes,
                path: String::from("/workspace/note.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: Some(1_700_000_000_000),
                mtime_ms: Some(1_710_000_000_000),
                len: None,
            }),
        ),
    );
    let utimes = recv_response(&mut stdout, &codec, 11, &mut buffered_events);
    match utimes.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Utimes);
            assert_eq!(response.path, "/workspace/note.txt");
        }
        other => panic!("unexpected utimes response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            12,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Stat,
                path: String::from("/workspace/note.txt"),
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
        ),
    );
    let stat = recv_response(&mut stdout, &codec, 12, &mut buffered_events);
    match stat.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            let stat = response.stat.expect("stat payload");
            assert_eq!(stat.size, 5);
            assert_eq!(stat.atime_ms, 1_700_000_000_000);
            assert_eq!(stat.mtime_ms, 1_710_000_000_000);
            assert!(stat.nlink >= 2);
        }
        other => panic!("unexpected stat response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            13,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SnapshotRootFilesystem(SnapshotRootFilesystemRequest::default()),
        ),
    );
    let snapshot = recv_response(&mut stdout, &codec, 13, &mut buffered_events);
    match snapshot.payload {
        ResponsePayload::RootFilesystemSnapshot(response) => {
            assert!(response
                .entries
                .iter()
                .any(|entry| entry.path == "/workspace/note.txt"));
        }
        other => panic!("unexpected snapshot response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            14,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: String::from("proc-1"),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: String::from("./entry.mjs"),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
            }),
        ),
    );
    let started = recv_response(&mut stdout, &codec, 14, &mut buffered_events);
    match started.payload {
        ResponsePayload::ProcessStarted(response) => {
            assert_eq!(response.process_id, "proc-1");
        }
        other => panic!("unexpected execute response: {other:?}"),
    }

    let (stdout_text, stderr_text, exit_code) =
        collect_process_events(&mut stdout, &codec, "proc-1");
    assert!(
        stdout_text.contains("stdio-binary-ok"),
        "stdout was {stdout_text:?}"
    );
    assert_eq!(stderr_text, "");
    assert_eq!(exit_code, 0);

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            15,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVm(agent_os_sidecar::protocol::DisposeVmRequest {
                reason: agent_os_sidecar::protocol::DisposeReason::Requested,
            }),
        ),
    );
    let disposed = recv_response(&mut stdout, &codec, 15, &mut buffered_events);
    match disposed.payload {
        ResponsePayload::VmDisposed(response) => assert_eq!(response.vm_id, vm_id),
        other => panic!("unexpected dispose response: {other:?}"),
    }

    drop(stdin);
    let status = child.wait().expect("wait for sidecar child");
    assert!(status.success(), "sidecar binary exited with {status}");
}

#[test]
fn native_sidecar_binary_supports_js_bridge_host_filesystem_access() {
    let host_root = temp_dir("stdio-binary-host-bridge");
    fs::write(host_root.join("existing.txt"), "host-bridge-ok").expect("seed host file");

    let (mut child, mut stdin, mut stdout) = spawn_sidecar_binary();
    let codec = NativeFrameCodec::default();
    let mut buffered_events = Vec::new();

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            1,
            OwnershipScope::connection("client-hint"),
            RequestPayload::Authenticate(AuthenticateRequest {
                client_name: String::from("stdio-test"),
                auth_token: String::from("stdio-test-token"),
            }),
        ),
    );
    let authenticated = recv_response(&mut stdout, &codec, 1, &mut buffered_events);
    let connection_id = match authenticated.payload {
        ResponsePayload::Authenticated(response) => response.connection_id,
        other => panic!("unexpected authenticate response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            2,
            OwnershipScope::connection(&connection_id),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: SidecarPlacement::Shared { pool: None },
                metadata: BTreeMap::new(),
            }),
        ),
    );
    let session_opened = recv_response(&mut stdout, &codec, 2, &mut buffered_events);
    let session_id = match session_opened.payload {
        ResponsePayload::SessionOpened(response) => response.session_id,
        other => panic!("unexpected open-session response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            3,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::new(),
                root_filesystem: Default::default(),
                permissions: Vec::new(),
            }),
        ),
    );
    let created = recv_response(&mut stdout, &codec, 3, &mut buffered_events);
    let vm_id = match created.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected create-vm response: {other:?}"),
    };
    let lifecycle_states = collect_vm_lifecycle_states(&mut stdout, &codec, 2);
    assert_eq!(
        lifecycle_states,
        vec![
            agent_os_sidecar::protocol::VmLifecycleState::Creating,
            agent_os_sidecar::protocol::VmLifecycleState::Ready,
        ]
    );

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: host_root.to_string_lossy().into_owned(),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("js_bridge"),
                        config: json!({}),
                    },
                }],
                software: Vec::new(),
                permissions: Vec::new(),
                instructions: Vec::new(),
                projected_modules: Vec::new(),
            }),
        ),
    );
    let configured = recv_response(&mut stdout, &codec, 4, &mut buffered_events);
    match configured.payload {
        ResponsePayload::VmConfigured(response) => {
            assert_eq!(response.applied_mounts, 1);
            assert_eq!(response.applied_software, 0);
        }
        other => panic!("unexpected configure response: {other:?}"),
    }

    let existing_path = format!("{}/existing.txt", host_root.to_string_lossy());
    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: existing_path,
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
        ),
    );
    let read = recv_response(&mut stdout, &codec, 5, &mut buffered_events);
    match read.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.content.as_deref(), Some("host-bridge-ok"));
        }
        other => panic!("unexpected read response: {other:?}"),
    }

    let generated_path = format!("{}/generated.txt", host_root.to_string_lossy());
    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: generated_path,
                destination_path: None,
                target: None,
                content: Some(String::from("from-js-bridge")),
                encoding: None,
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ),
    );
    let write = recv_response(&mut stdout, &codec, 6, &mut buffered_events);
    match write.payload {
        ResponsePayload::GuestFilesystemResult(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
        }
        other => panic!("unexpected write response: {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(host_root.join("generated.txt")).expect("read generated host file"),
        "from-js-bridge"
    );

    send_request(
        &mut stdin,
        &codec,
        RequestFrame::new(
            7,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVm(agent_os_sidecar::protocol::DisposeVmRequest {
                reason: agent_os_sidecar::protocol::DisposeReason::Requested,
            }),
        ),
    );
    let disposed = recv_response(&mut stdout, &codec, 7, &mut buffered_events);
    match disposed.payload {
        ResponsePayload::VmDisposed(response) => assert_eq!(response.vm_id, vm_id),
        other => panic!("unexpected dispose response: {other:?}"),
    }

    drop(stdin);
    let status = child.wait().expect("wait for sidecar child");
    assert!(status.success(), "sidecar binary exited with {status}");
}
