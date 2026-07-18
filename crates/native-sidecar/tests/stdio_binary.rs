mod support;

use agentos_native_sidecar::wire::{self, *};
use base64::Engine;
use command_fds::{CommandFdExt, FdMapping};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};
use support::{
    temp_dir, wire_connection, wire_permissions_allow_all, wire_request, wire_session, wire_vm,
};

const MAX_STDIO_BINARY_PROCESS_STREAM_BYTES: usize = DEFAULT_MAX_FRAME_BYTES;

fn root_filesystem_descriptor() -> RootFilesystemDescriptor {
    RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: false,
        lowers: Vec::new(),
        bootstrap_entries: Vec::new(),
    }
}

fn send_request(stdin: &mut ChildStdin, codec: &WireFrameCodec, request: RequestFrame) {
    let encoded = codec
        .encode(&ProtocolFrame::RequestFrame(request))
        .expect("encode request");
    stdin.write_all(&encoded).expect("write request");
    stdin.flush().expect("flush request");
}

fn declared_frame_payload_len(prefix: &[u8; 4], codec: &WireFrameCodec) -> usize {
    let declared = u32::from_be_bytes(*prefix) as usize;
    assert!(
        declared <= codec.max_frame_bytes(),
        "declared frame payload {declared} exceeds {} byte limit",
        codec.max_frame_bytes()
    );
    declared
}

fn read_frame(reader: &mut impl Read, codec: &WireFrameCodec) -> ProtocolFrame {
    let mut prefix = [0u8; 4];
    reader.read_exact(&mut prefix).expect("read length prefix");
    let declared = declared_frame_payload_len(&prefix, codec);
    let mut bytes = Vec::with_capacity(4 + declared);
    bytes.extend_from_slice(&prefix);
    bytes.resize(4 + declared, 0);
    reader
        .read_exact(&mut bytes[4..])
        .expect("read framed payload");
    codec.decode(&bytes).expect("decode frame")
}

fn recv_response(
    control: &mut UnixStream,
    codec: &WireFrameCodec,
    request_id: RequestId,
    events: &mut Vec<EventPayload>,
) -> ResponseFrame {
    loop {
        match read_frame(control, codec) {
            ProtocolFrame::ResponseFrame(response) if response.request_id == request_id => {
                return response;
            }
            ProtocolFrame::EventFrame(event) => events.push(event.payload),
            other => panic!("unexpected frame while waiting for response {request_id}: {other:?}"),
        }
    }
}

fn send_sidecar_response(
    control: &mut UnixStream,
    codec: &WireFrameCodec,
    response: SidecarResponseFrame,
) {
    let encoded = codec
        .encode(&ProtocolFrame::SidecarResponseFrame(response))
        .expect("encode sidecar response");
    control
        .write_all(&encoded)
        .expect("write sidecar response frame");
    control.flush().expect("flush sidecar response frame");
}

fn send_shutdown(control: &mut UnixStream, codec: &WireFrameCodec, reason: &str) {
    let encoded = codec
        .encode(&ProtocolFrame::ControlFrame(ControlFrame {
            schema: wire::protocol_schema(),
            payload: ControlPayload::ShutdownControl(ShutdownControl {
                reason: reason.to_owned(),
            }),
        }))
        .expect("encode shutdown control frame");
    control
        .write_all(&encoded)
        .expect("write shutdown control frame");
    control.flush().expect("flush shutdown control frame");
}

fn recv_response_with_sidecar_handler(
    control: &mut UnixStream,
    codec: &WireFrameCodec,
    request_id: RequestId,
    events: &mut Vec<EventPayload>,
    mut handle: impl FnMut(&SidecarRequestFrame) -> SidecarResponsePayload,
) -> ResponseFrame {
    loop {
        match read_frame(control, codec) {
            ProtocolFrame::ResponseFrame(response) if response.request_id == request_id => {
                return response;
            }
            ProtocolFrame::EventFrame(event) => events.push(event.payload),
            ProtocolFrame::SidecarRequestFrame(request) => {
                let payload = handle(&request);
                send_sidecar_response(
                    control,
                    codec,
                    SidecarResponseFrame {
                        schema: wire::protocol_schema(),
                        request_id: request.request_id,
                        ownership: request.ownership.clone(),
                        payload,
                    },
                );
            }
            other => panic!("unexpected frame while waiting for response {request_id}: {other:?}"),
        }
    }
}

fn js_bridge_args(call: &JsBridgeCallRequest) -> serde_json::Value {
    serde_json::from_str(&call.args).expect("parse js bridge args")
}

fn js_bridge_result(
    call: &JsBridgeCallRequest,
    result: Option<serde_json::Value>,
    error: Option<String>,
) -> SidecarResponsePayload {
    SidecarResponsePayload::JsBridgeResultResponse(JsBridgeResultResponse {
        call_id: call.call_id.clone(),
        result: result.map(|value| value.to_string()),
        error,
    })
}

fn js_bridge_root_response(call: &JsBridgeCallRequest) -> Option<SidecarResponsePayload> {
    let args = js_bridge_args(call);
    if args["path"].as_str() != Some("/") {
        return None;
    }
    match call.operation.as_str() {
        "exists" => Some(js_bridge_result(
            call,
            Some(serde_json::Value::Bool(true)),
            None,
        )),
        "stat" | "lstat" => Some(js_bridge_result(
            call,
            Some(json!({
                "mode": 0o755,
                "size": 0,
                "blocks": 0,
                "dev": 1,
                "rdev": 0,
                "isDirectory": true,
                "isSymbolicLink": false,
                "atimeMs": 0,
                "mtimeMs": 0,
                "ctimeMs": 0,
                "birthtimeMs": 0,
                "ino": 1,
                "nlink": 1,
                "uid": 0,
                "gid": 0,
            })),
            None,
        )),
        "readDir" => Some(js_bridge_result(call, Some(json!([])), None)),
        "readDirWithTypes" => Some(js_bridge_result(call, Some(json!([])), None)),
        "realpath" => Some(js_bridge_result(call, Some(json!("/")), None)),
        _ => None,
    }
}

fn append_process_stream_chunk(stream: &mut Vec<u8>, chunk: &[u8], stream_name: &str) {
    assert!(
        stream.len().saturating_add(chunk.len()) <= MAX_STDIO_BINARY_PROCESS_STREAM_BYTES,
        "{stream_name} exceeded {MAX_STDIO_BINARY_PROCESS_STREAM_BYTES} bytes"
    );
    stream.extend_from_slice(chunk);
}

fn process_stream_to_string(stream: &[u8]) -> String {
    String::from_utf8_lossy(stream).into_owned()
}

fn collect_process_events(
    stdout: &mut ChildStdout,
    codec: &WireFrameCodec,
    process_id: &str,
) -> (String, String, i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut stdout_text = Vec::new();
    let mut stderr_text = Vec::new();

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process events"
        );
        match read_frame(stdout, codec) {
            ProtocolFrame::EventFrame(event) => match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => {
                            append_process_stream_chunk(&mut stdout_text, &output.chunk, "stdout");
                        }
                        StreamChannel::Stderr => {
                            append_process_stream_chunk(&mut stderr_text, &output.chunk, "stderr");
                        }
                    }
                }
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    return (
                        process_stream_to_string(&stdout_text),
                        process_stream_to_string(&stderr_text),
                        exited.exit_code,
                    );
                }
                _ => {}
            },
            other => panic!("unexpected frame while waiting for process events: {other:?}"),
        }
    }
}

fn collect_vm_lifecycle_states(
    stdout: &mut ChildStdout,
    codec: &WireFrameCodec,
    count: usize,
) -> Vec<VmLifecycleState> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut states = Vec::new();

    while states.len() < count {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for VM lifecycle events"
        );
        match read_frame(stdout, codec) {
            ProtocolFrame::EventFrame(event) => {
                if let EventPayload::VmLifecycleEvent(lifecycle) = event.payload {
                    states.push(lifecycle.state);
                }
            }
            other => panic!("unexpected frame while waiting for lifecycle events: {other:?}"),
        }
    }

    states
}

fn spawn_sidecar_binary() -> (Child, ChildStdin, ChildStdout, UnixStream) {
    let (control, child_control) = UnixStream::pair().expect("create control socketpair");
    let mut command = Command::new(env!("CARGO_BIN_EXE_agentos-native-sidecar"));
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .fd_mappings(vec![FdMapping {
            parent_fd: child_control.into(),
            child_fd: 3,
        }])
        .expect("map control socket to fd 3");
    let mut child = command.spawn().expect("spawn native sidecar binary");
    let stdin = child.stdin.take().expect("capture sidecar stdin");
    let stdout = child.stdout.take().expect("capture sidecar stdout");
    (child, stdin, stdout, control)
}

fn wait_for_child(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("poll sidecar child") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for sidecar child"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn write_script(root: &Path) {
    fs::write(root.join("entry.mjs"), "console.log('stdio-binary-ok');\n")
        .expect("write test entrypoint");
}

#[test]
fn stdio_binary_test_helpers_bound_frame_and_stream_buffers() {
    let codec = WireFrameCodec::default();
    let max_prefix = (codec.max_frame_bytes() as u32).to_be_bytes();
    assert_eq!(
        declared_frame_payload_len(&max_prefix, &codec),
        codec.max_frame_bytes()
    );

    let oversized_prefix = ((codec.max_frame_bytes() + 1) as u32).to_be_bytes();
    let oversized_frame = std::panic::catch_unwind(|| {
        declared_frame_payload_len(&oversized_prefix, &codec);
    });
    assert!(
        oversized_frame.is_err(),
        "oversized frame payload should fail before allocation"
    );

    let mut stream = Vec::new();
    append_process_stream_chunk(&mut stream, &[b'a'; 16], "stdout");
    assert_eq!(stream.len(), 16);

    let oversized_stream = std::panic::catch_unwind(|| {
        let mut stream = vec![b'a'; MAX_STDIO_BINARY_PROCESS_STREAM_BYTES];
        append_process_stream_chunk(&mut stream, b"!", "stdout");
    });
    assert!(
        oversized_stream.is_err(),
        "oversized process stream should fail before appending"
    );
}

#[test]
fn closing_either_required_ingress_stream_is_terminal() {
    let (mut ordinary_child, ordinary_stdin, _ordinary_stdout, _ordinary_control) =
        spawn_sidecar_binary();
    drop(ordinary_stdin);
    assert!(
        wait_for_child(&mut ordinary_child).success(),
        "ordinary EOF should gracefully terminate the sidecar"
    );

    let (mut control_child, control_stdin, _control_stdout, control) = spawn_sidecar_binary();
    drop(control);
    let status = wait_for_child(&mut control_child);
    drop(control_stdin);
    assert!(
        !status.success(),
        "unexpected response/control EOF should fail the sidecar"
    );
}

#[test]
fn native_sidecar_binary_runs_the_framed_protocol_over_stdio() {
    let temp = temp_dir("stdio-binary");
    write_script(&temp);

    let (mut child, mut stdin, mut stdout, mut control) = spawn_sidecar_binary();
    let codec = WireFrameCodec::default();
    let mut buffered_events = Vec::new();

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            1,
            wire_connection("client-hint"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("stdio-test"),
                auth_token: String::from("stdio-test-token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ),
    );
    let authenticated = recv_response(&mut control, &codec, 1, &mut buffered_events);
    let connection_id = match authenticated.payload {
        ResponsePayload::AuthenticatedResponse(response) => response.connection_id,
        other => panic!("unexpected authenticate response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            2,
            wire_connection(&connection_id),
            RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: HashMap::new(),
            }),
        ),
    );
    let session_opened = recv_response(&mut control, &codec, 2, &mut buffered_events);
    let session_id = match session_opened.payload {
        ResponsePayload::SessionOpenedResponse(response) => response.session_id,
        other => panic!("unexpected open-session response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::from([(String::from("cwd"), temp.to_string_lossy().into_owned())]),
                root_filesystem_descriptor(),
                Some(wire_permissions_allow_all()),
            )),
        ),
    );
    let created = recv_response(&mut control, &codec, 3, &mut buffered_events);
    let vm_id = match created.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create-vm response: {other:?}"),
    };
    let lifecycle_states = collect_vm_lifecycle_states(&mut stdout, &codec, 2);
    assert_eq!(
        lifecycle_states,
        vec![VmLifecycleState::Creating, VmLifecycleState::Ready,]
    );

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Mkdir,
                path: String::from("/workspace"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: true,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
                offset: None,
            }),
        ),
    );
    let mkdir = recv_response(&mut control, &codec, 4, &mut buffered_events);
    match mkdir.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.path, "/workspace");
            assert_eq!(response.operation, GuestFilesystemOperation::Mkdir);
        }
        other => panic!("unexpected mkdir response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/note.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("stdio-sidecar-fs")),
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
        ),
    );
    let write = recv_response(&mut control, &codec, 5, &mut buffered_events);
    match write.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.path, "/workspace/note.txt");
            assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
        }
        other => panic!("unexpected write response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/note.txt"),
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
        ),
    );
    let read = recv_response(&mut control, &codec, 6, &mut buffered_events);
    match read.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.content.as_deref(), Some("stdio-sidecar-fs"));
        }
        other => panic!("unexpected read response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Symlink,
                path: String::from("/workspace/link.txt"),
                destination_path: None,
                target: Some(String::from("/workspace/note.txt")),
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
        ),
    );
    let symlink = recv_response(&mut control, &codec, 7, &mut buffered_events);
    match symlink.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Symlink);
            assert_eq!(response.target.as_deref(), Some("/workspace/note.txt"));
        }
        other => panic!("unexpected symlink response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            8,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Realpath,
                path: String::from("/workspace/link.txt"),
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
        ),
    );
    let realpath = recv_response(&mut control, &codec, 8, &mut buffered_events);
    match realpath.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Realpath);
            assert_eq!(response.target.as_deref(), Some("/workspace/note.txt"));
        }
        other => panic!("unexpected realpath response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            9,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Link,
                path: String::from("/workspace/note.txt"),
                destination_path: Some(String::from("/workspace/hard.txt")),
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
        ),
    );
    let link = recv_response(&mut control, &codec, 9, &mut buffered_events);
    match link.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Link);
            assert_eq!(response.target.as_deref(), Some("/workspace/hard.txt"));
        }
        other => panic!("unexpected link response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            10,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Truncate,
                path: String::from("/workspace/hard.txt"),
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
                len: Some(5),
                offset: None,
            }),
        ),
    );
    let truncate = recv_response(&mut control, &codec, 10, &mut buffered_events);
    match truncate.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Truncate);
            assert_eq!(response.path, "/workspace/hard.txt");
        }
        other => panic!("unexpected truncate response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            11,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Utimes,
                path: String::from("/workspace/note.txt"),
                destination_path: None,
                target: None,
                content: None,
                encoding: None,
                recursive: false,
                max_depth: None,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: Some(1_700_000_000_000),
                mtime_ms: Some(1_710_000_000_000),
                len: None,
                offset: None,
            }),
        ),
    );
    let utimes = recv_response(&mut control, &codec, 11, &mut buffered_events);
    match utimes.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.operation, GuestFilesystemOperation::Utimes);
            assert_eq!(response.path, "/workspace/note.txt");
        }
        other => panic!("unexpected utimes response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            12,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::Stat,
                path: String::from("/workspace/note.txt"),
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
        ),
    );
    let stat = recv_response(&mut control, &codec, 12, &mut buffered_events);
    match stat.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
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
        wire_request(
            13,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SnapshotRootFilesystemRequest(SnapshotRootFilesystemRequest {
                max_bytes: 64 * 1024 * 1024,
            }),
        ),
    );
    let snapshot = recv_response(&mut control, &codec, 13, &mut buffered_events);
    match snapshot.payload {
        ResponsePayload::RootFilesystemSnapshotResponse(response) => {
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
        wire_request(
            14,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: String::from("proc-1"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(String::from("./entry.mjs")),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ),
    );
    let started = recv_response(&mut control, &codec, 14, &mut buffered_events);
    match started.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
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
        wire_request(
            15,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ),
    );
    let disposed = recv_response(&mut control, &codec, 15, &mut buffered_events);
    match disposed.payload {
        ResponsePayload::VmDisposedResponse(response) => assert_eq!(response.vm_id, vm_id),
        other => panic!("unexpected dispose response: {other:?}"),
    }

    send_shutdown(&mut control, &codec, "stdio binary test complete");
    let status = child.wait().expect("wait for sidecar child");
    assert!(status.success(), "sidecar binary exited with {status}");
}

#[test]
fn native_sidecar_binary_supports_js_bridge_host_filesystem_access() {
    let host_root = temp_dir("stdio-binary-host-bridge");
    fs::write(host_root.join("existing.txt"), "host-bridge-ok").expect("seed host file");

    let (mut child, mut stdin, mut stdout, mut control) = spawn_sidecar_binary();
    let codec = WireFrameCodec::default();
    let mut buffered_events = Vec::new();

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            1,
            wire_connection("client-hint"),
            RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("stdio-test"),
                auth_token: String::from("stdio-test-token"),
                protocol_version: wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        ),
    );
    let authenticated = recv_response(&mut control, &codec, 1, &mut buffered_events);
    let connection_id = match authenticated.payload {
        ResponsePayload::AuthenticatedResponse(response) => response.connection_id,
        other => panic!("unexpected authenticate response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            2,
            wire_connection(&connection_id),
            RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: HashMap::new(),
            }),
        ),
    );
    let session_opened = recv_response(&mut control, &codec, 2, &mut buffered_events);
    let session_id = match session_opened.payload {
        ResponsePayload::SessionOpenedResponse(response) => response.session_id,
        other => panic!("unexpected open-session response: {other:?}"),
    };

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            3,
            wire_session(&connection_id, &session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                GuestRuntimeKind::JavaScript,
                HashMap::new(),
                root_filesystem_descriptor(),
                Some(wire_permissions_allow_all()),
            )),
        ),
    );
    let created = recv_response(&mut control, &codec, 3, &mut buffered_events);
    let vm_id = match created.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create-vm response: {other:?}"),
    };
    let lifecycle_states = collect_vm_lifecycle_states(&mut stdout, &codec, 2);
    assert_eq!(
        lifecycle_states,
        vec![VmLifecycleState::Creating, VmLifecycleState::Ready,]
    );

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    guest_source: String::from("js_bridge"),
                    guest_fstype: String::from("js_bridge"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("js_bridge"),
                        config: json!({ "mountId": "mount-1" }).to_string(),
                    },
                }],
                software: Vec::new(),
                permissions: Some(wire_permissions_allow_all()),
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
        ),
    );
    let configured = recv_response(&mut control, &codec, 4, &mut buffered_events);
    match configured.payload {
        ResponsePayload::VmConfiguredResponse(response) => {
            // 1 = just the client mount. With no packages configured there are no
            // granular /opt/agentos leaf mounts (one tar/bin/current mount is added
            // per package, not a single always-present staging mount).
            assert_eq!(response.applied_mounts, 1);
            assert_eq!(response.applied_software, 0);
        }
        other => panic!("unexpected configure response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::ReadFile,
                path: String::from("/workspace/existing.txt"),
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
        ),
    );
    let read = recv_response_with_sidecar_handler(
        &mut control,
        &codec,
        5,
        &mut buffered_events,
        |request| {
            assert_eq!(
                request.ownership,
                wire_vm(&connection_id, &session_id, &vm_id)
            );
            let SidecarRequestPayload::JsBridgeCallRequest(call) = &request.payload else {
                panic!("expected js_bridge_call payload");
            };
            assert_eq!(call.mount_id, "mount-1");
            if let Some(response) = js_bridge_root_response(call) {
                return response;
            }
            let args = js_bridge_args(call);
            match (
                call.operation.as_str(),
                args["path"].as_str().expect("read path"),
            ) {
                ("exists", "/existing.txt") => {
                    js_bridge_result(call, Some(serde_json::Value::Bool(true)), None)
                }
                ("realpath", "/existing.txt") => {
                    js_bridge_result(call, Some(json!("/existing.txt")), None)
                }
                ("stat" | "lstat", "/existing.txt") => {
                    let metadata = fs::metadata(host_root.join("existing.txt"))
                        .expect("stat existing host file");
                    js_bridge_result(
                        call,
                        Some(json!({
                            "mode": 0o644,
                            "size": metadata.len(),
                            "blocks": 0,
                            "dev": 1,
                            "rdev": 0,
                            "isDirectory": false,
                            "isSymbolicLink": false,
                            "atimeMs": 0,
                            "mtimeMs": 0,
                            "ctimeMs": 0,
                            "birthtimeMs": 0,
                            "ino": 2,
                            "nlink": 1,
                            "uid": 0,
                            "gid": 0,
                        })),
                        None,
                    )
                }
                ("readFile", "/existing.txt") => js_bridge_result(
                    call,
                    Some(serde_json::Value::String(
                        base64::engine::general_purpose::STANDARD.encode(
                            fs::read(host_root.join("existing.txt")).expect("read host file"),
                        ),
                    )),
                    None,
                ),
                ("utimes", "/existing.txt") => {
                    js_bridge_result(call, Some(serde_json::Value::Null), None)
                }
                other => panic!("unexpected js bridge read callback: {other:?}"),
            }
        },
    );
    match read.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
            assert_eq!(response.content.as_deref(), Some("host-bridge-ok"));
        }
        other => panic!("unexpected read response: {other:?}"),
    }

    send_request(
        &mut stdin,
        &codec,
        wire_request(
            6,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCallRequest(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/workspace/generated.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("from-js-bridge")),
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
        ),
    );
    let write = recv_response_with_sidecar_handler(
        &mut control,
        &codec,
        6,
        &mut buffered_events,
        |request| {
            let SidecarRequestPayload::JsBridgeCallRequest(call) = &request.payload else {
                panic!("expected js_bridge_call payload");
            };
            assert_eq!(call.mount_id, "mount-1");
            if let Some(response) = js_bridge_root_response(call) {
                return response;
            }
            let args = js_bridge_args(call);
            if args["path"].as_str() == Some("/generated.txt") {
                let generated_path = host_root.join("generated.txt");
                match call.operation.as_str() {
                    "exists" => {
                        return js_bridge_result(
                            call,
                            Some(serde_json::Value::Bool(generated_path.exists())),
                            None,
                        );
                    }
                    "stat" | "lstat" => {
                        if let Ok(metadata) = fs::metadata(&generated_path) {
                            return js_bridge_result(
                                call,
                                Some(json!({
                                    "mode": 0o644,
                                    "size": metadata.len(),
                                    "blocks": 0,
                                    "dev": 1,
                                    "rdev": 0,
                                    "isDirectory": false,
                                    "isSymbolicLink": false,
                                    "atimeMs": 0,
                                    "mtimeMs": 0,
                                    "ctimeMs": 0,
                                    "birthtimeMs": 0,
                                    "ino": 2,
                                    "nlink": 1,
                                    "uid": 0,
                                    "gid": 0,
                                })),
                                None,
                            );
                        }
                        return js_bridge_result(call, None, Some(String::from("not found")));
                    }
                    "realpath" => {
                        return js_bridge_result(call, None, Some(String::from("not found")));
                    }
                    _ => {}
                }
            }
            match (
                call.operation.as_str(),
                args["path"].as_str().expect("write path"),
            ) {
                ("realpath", "/generated.txt") => {
                    js_bridge_result(call, None, Some(String::from("not found")))
                }
                ("writeFile", "/generated.txt") => {
                    let content = base64::engine::general_purpose::STANDARD
                        .decode(args["content"].as_str().expect("write content"))
                        .expect("decode js bridge write");
                    fs::write(host_root.join("generated.txt"), content).expect("write host file");
                    js_bridge_result(call, None, None)
                }
                other => panic!("unexpected js bridge write callback: {other:?}"),
            }
        },
    );
    match write.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => {
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
        wire_request(
            7,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ),
    );
    let mut ordinary_frame_blocked = false;
    let disposed = recv_response_with_sidecar_handler(
        &mut control,
        &codec,
        7,
        &mut buffered_events,
        |request| {
            let SidecarRequestPayload::JsBridgeCallRequest(call) = &request.payload else {
                panic!("expected js_bridge_call payload during dispose");
            };
            assert_eq!(call.mount_id, "mount-1");
            if !ordinary_frame_blocked {
                // Leave a legal-size ordinary frame incomplete ahead of any
                // later fd0 bytes. The callback reply and its DisposeVm
                // response must still make progress on the independent fd3
                // response/control stream.
                stdin
                    .write_all(&64_u32.to_be_bytes())
                    .expect("write incomplete ordinary frame prefix");
                stdin
                    .write_all(b"partial")
                    .expect("write incomplete ordinary frame body");
                stdin.flush().expect("flush incomplete ordinary frame");
                ordinary_frame_blocked = true;
            }
            js_bridge_root_response(call)
                .unwrap_or_else(|| panic!("unexpected js bridge dispose callback: {call:?}"))
        },
    );
    match disposed.payload {
        ResponsePayload::VmDisposedResponse(response) => assert_eq!(response.vm_id, vm_id),
        other => panic!("unexpected dispose response: {other:?}"),
    }

    send_shutdown(&mut control, &codec, "stdio bridge test complete");
    let status = child.wait().expect("wait for sidecar child");
    assert!(status.success(), "sidecar binary exited with {status}");
}
