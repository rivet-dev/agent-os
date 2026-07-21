mod support;

use agentos_native_sidecar::wire::{
    BootstrapRootFilesystemRequest, CloseStdinRequest, ConfigureVmRequest, CreateVmRequest,
    EventPayload, ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestRuntimeKind, KillProcessRequest, MountDescriptor, MountPluginDescriptor, OwnershipScope,
    PatternPermissionRule, PatternPermissionRuleSet, PatternPermissionScope, PermissionMode,
    PermissionsPolicy, RequestId, RequestPayload, ResponsePayload, RootFilesystemDescriptor,
    RootFilesystemEntry, RootFilesystemEntryEncoding, RootFilesystemEntryKind, RootFilesystemMode,
    StreamChannel, WriteStdinRequest,
};
use nix::libc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, UdpSocket};
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate_wire, create_vm_wire, new_sidecar, open_session_wire,
    temp_dir, wire_permissions_allow_all, wire_request, wire_session, wire_vm, write_fixture,
};

const MAX_PROCESS_STREAM_BYTES: usize = 1024 * 1024;

#[derive(Debug, Default)]
struct ProcessResult {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
}

fn append_stream_chunk(stream: &mut Vec<u8>, chunk: &[u8], label: &str) {
    assert!(
        stream.len().saturating_add(chunk.len()) <= MAX_PROCESS_STREAM_BYTES,
        "{label} exceeded {MAX_PROCESS_STREAM_BYTES} bytes"
    );
    stream.extend_from_slice(chunk);
}

fn chunk_contains(chunk: &[u8], needle: &str) -> bool {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return true;
    }
    chunk.windows(needle.len()).any(|window| window == needle)
}

fn root_dir(path: impl Into<String>) -> RootFilesystemEntry {
    let mut entry = root_entry(path, RootFilesystemEntryKind::Directory, None, None);
    entry.uid = Some(1000);
    entry.gid = Some(1000);
    entry
}

fn root_file(
    path: impl Into<String>,
    content: impl Into<String>,
    encoding: Option<RootFilesystemEntryEncoding>,
) -> RootFilesystemEntry {
    root_entry(
        path,
        RootFilesystemEntryKind::File,
        Some(content.into()),
        encoding,
    )
}

fn root_entry(
    path: impl Into<String>,
    kind: RootFilesystemEntryKind,
    content: Option<String>,
    encoding: Option<RootFilesystemEntryEncoding>,
) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.into(),
        kind,
        mode: None,
        uid: None,
        gid: None,
        content,
        encoding,
        target: None,
        executable: false,
    }
}

fn collect_process_output(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> (String, String, i32) {
    collect_process_output_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        Duration::from_secs(10),
    )
}

fn collect_process_output_with_timeout(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    timeout: Duration,
) -> (String, String, i32) {
    let ownership = wire_session(connection_id, session_id);
    let deadline = Instant::now() + timeout;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit = None;

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for process events\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar wire event");
        if let Some(event) = event {
            assert_eq!(event.ownership, wire_vm(connection_id, session_id, vm_id));

            match event.payload {
                EventPayload::ProcessOutputEvent(output) if output.process_id == process_id => {
                    match output.channel {
                        StreamChannel::Stdout => {
                            append_stream_chunk(&mut stdout, &output.chunk, "stdout");
                        }
                        StreamChannel::Stderr => {
                            append_stream_chunk(&mut stderr, &output.chunk, "stderr");
                        }
                    }
                }
                EventPayload::ProcessOutputEvent(_) => {}
                EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                    exit = Some((exited.exit_code, Instant::now()));
                }
                EventPayload::ProcessExitedEvent(_)
                | EventPayload::VmLifecycleEvent(_)
                | EventPayload::StructuredEvent(_)
                | EventPayload::ExtEnvelope(_) => {}
            }
        }

        if let Some((exit_code, seen_at)) = exit {
            if Instant::now().duration_since(seen_at) >= Duration::from_millis(200) {
                return (
                    String::from_utf8_lossy(&stdout).into_owned(),
                    String::from_utf8_lossy(&stderr).into_owned(),
                    exit_code,
                );
            }
        }
    }
}

fn pyodide_asset_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sidecar crate parent")
        .join("execution")
        .join("assets")
        .join("pyodide")
}

fn static_file_path(root: &Path, request_target: &str) -> Option<PathBuf> {
    let path = request_target.split('?').next().unwrap_or(request_target);
    let mut resolved = root.to_path_buf();
    for component in Path::new(path.trim_start_matches('/')).components() {
        match component {
            Component::Normal(segment) => resolved.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

fn static_file_server_rejects_traversal_paths() {
    let root = Path::new("/tmp/pyodide-assets");
    assert_eq!(
        static_file_path(root, "/click-8.3.1-py3-none-any.whl?download=1"),
        Some(root.join("click-8.3.1-py3-none-any.whl"))
    );
    assert_eq!(static_file_path(root, "/../secret.txt"), None);
    assert_eq!(static_file_path(root, "/packages/../../secret.txt"), None);
}

fn spawn_tcp_echo_server() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tcp echo listener");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking echo listener");
    let port = listener.local_addr().expect("echo listener address").port();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(180);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
                    let mut buf = [0_u8; 4096];
                    loop {
                        match stream.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if stream.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });
    (port, handle)
}

fn spawn_udp_echo_server() -> (u16, thread::JoinHandle<()>) {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind udp echo socket");
    socket
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set udp echo read timeout");
    let port = socket.local_addr().expect("udp echo address").port();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut buf = [0_u8; 4096];
        while Instant::now() < deadline {
            match socket.recv_from(&mut buf) {
                Ok((n, addr)) => {
                    let _ = socket.send_to(&buf[..n], addr);
                    return;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
    });
    (port, handle)
}

fn spawn_static_file_server(root: PathBuf) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind static file listener");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking listener");
    let port = listener.local_addr().expect("listener address").port();
    let handle = thread::spawn(move || {
        // Generous windows so a slow/contended Pyodide boot (and micropip's
        // index-then-wheel fetch gap) still lands inside the server's lifetime.
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut served_any = false;
        let mut idle_since: Option<Instant> = None;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("set static file stream read timeout");
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .expect("set static file stream write timeout");
                    served_any = true;
                    idle_since = None;
                    let mut request = [0_u8; 4096];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let request_text = String::from_utf8_lossy(&request[..read]);
                    let path = request_text
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .unwrap_or("/");
                    let (status_line, body) = match static_file_path(&root, path) {
                        Some(file_path) => match fs::read(&file_path) {
                            Ok(body) => ("HTTP/1.1 200 OK", body),
                            Err(_) => ("HTTP/1.1 404 Not Found", b"missing".to_vec()),
                        },
                        None => ("HTTP/1.1 400 Bad Request", b"bad request".to_vec()),
                    };
                    let response = format!(
                        "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.write_all(&body);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if served_any {
                        match idle_since {
                            Some(start) if start.elapsed() >= Duration::from_secs(20) => break,
                            Some(_) => {}
                            None => idle_since = Some(Instant::now()),
                        }
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });
    (port, handle)
}

fn execute_inline_python(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    code: &str,
) {
    execute_python_entrypoint_with_env(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        code,
        HashMap::new(),
    );
}

#[allow(clippy::too_many_arguments)]
fn execute_inline_python_with_env(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    code: &str,
    env: HashMap<String, String>,
) {
    execute_python_entrypoint_with_env(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        code,
        env,
    );
}

fn execute_python_entrypoint(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &str,
) {
    execute_python_entrypoint_with_env(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        entrypoint,
        HashMap::new(),
    );
}

#[allow(clippy::too_many_arguments)]
fn execute_python_entrypoint_with_env(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &str,
    env: HashMap<String, String>,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: None,
                runtime: Some(GuestRuntimeKind::Python),
                entrypoint: Some(entrypoint.to_owned()),
                args: Vec::new(),
                env,
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: None,
            }),
        ))
        .expect("start python execution through wire");

    match result.response.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire execute response: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_javascript_with_env(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &Path,
    args: Vec<String>,
    env: HashMap<String, String>,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args,
                env,
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: None,
            }),
        ))
        .expect("start JavaScript execution through wire");

    match result.response.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire execute response: {other:?}"),
    }
}

fn create_vm_with_root_filesystem(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    root_filesystem: RootFilesystemDescriptor,
) -> String {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_session(connection_id, session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                runtime,
                HashMap::from([(String::from("cwd"), cwd.to_string_lossy().into_owned())]),
                root_filesystem,
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create sidecar VM through wire");

    match result.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected wire vm create response: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn create_vm_with_metadata_and_permissions(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    mut metadata: HashMap<String, String>,
    permissions: PermissionsPolicy,
) -> String {
    metadata
        .entry(String::from("cwd"))
        .or_insert_with(|| cwd.to_string_lossy().into_owned());

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_session(connection_id, session_id),
            RequestPayload::CreateVmRequest(CreateVmRequest::legacy_test_config(
                runtime,
                metadata,
                RootFilesystemDescriptor {
                    mode: RootFilesystemMode::Ephemeral,
                    disable_default_base_layer: false,
                    lowers: Vec::new(),
                    bootstrap_entries: Vec::new(),
                },
                Some(permissions),
            )),
        ))
        .expect("create sidecar VM through wire");

    match result.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected wire vm create response: {other:?}"),
    }
}

fn bootstrap_root_filesystem(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    entries: Vec<RootFilesystemEntry>,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::BootstrapRootFilesystemRequest(BootstrapRootFilesystemRequest {
                entries,
            }),
        ))
        .expect("bootstrap root filesystem through wire");

    match result.response.payload {
        ResponsePayload::RootFilesystemBootstrappedResponse(response) => {
            assert!(response.entry_count > 0);
        }
        other => panic!("unexpected wire bootstrap response: {other:?}"),
    }
}

fn guest_filesystem_call(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    payload: GuestFilesystemCallRequest,
) -> agentos_native_sidecar::wire::GuestFilesystemResultResponse {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::GuestFilesystemCallRequest(payload),
        ))
        .expect("guest filesystem call through wire");

    match result.response.payload {
        ResponsePayload::GuestFilesystemResultResponse(response) => response,
        other => panic!("unexpected wire guest filesystem response: {other:?}"),
    }
}

fn guest_write_file_utf8(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
    content: &str,
) {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::WriteFile,
            path: path.to_owned(),
            destination_path: None,
            target: None,
            content: Some(content.to_owned()),
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
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
    assert_eq!(response.path, path);
}

fn guest_read_file_utf8(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
) -> String {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::ReadFile,
            path: path.to_owned(),
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
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::ReadFile);
    assert_eq!(response.path, path);
    assert_eq!(response.encoding, Some(RootFilesystemEntryEncoding::Utf8));
    response.content.expect("guest filesystem read content")
}

fn guest_exists(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
) -> bool {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::Exists,
            path: path.to_owned(),
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
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::Exists);
    response.exists.expect("guest filesystem exists flag")
}

fn guest_readlink(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
) -> String {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::ReadLink,
            path: path.to_owned(),
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
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::ReadLink);
    response.target.expect("guest filesystem readlink target")
}

fn guest_symlink(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    link_path: &str,
    target: &str,
) {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::Symlink,
            path: link_path.to_owned(),
            destination_path: None,
            target: Some(target.to_owned()),
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
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::Symlink);
}

fn guest_stat_mode(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    path: &str,
) -> u32 {
    let response = guest_filesystem_call(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        GuestFilesystemCallRequest {
            operation: GuestFilesystemOperation::Stat,
            path: path.to_owned(),
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
        },
    );

    response.stat.expect("guest filesystem stat").mode
}

fn write_process_stdin(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    chunk: &str,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::WriteStdinRequest(WriteStdinRequest {
                process_id: process_id.to_owned(),
                chunk: chunk.as_bytes().to_vec(),
            }),
        ))
        .expect("write python stdin through wire");

    match result.response.payload {
        ResponsePayload::StdinWrittenResponse(response) => {
            assert_eq!(response.process_id, process_id);
            assert_eq!(response.accepted_bytes, chunk.len() as u64);
        }
        other => panic!("unexpected wire stdin-written response: {other:?}"),
    }
}

fn close_process_stdin(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::CloseStdinRequest(CloseStdinRequest {
                process_id: process_id.to_owned(),
            }),
        ))
        .expect("close python stdin through wire");

    match result.response.payload {
        ResponsePayload::StdinClosedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire stdin-closed response: {other:?}"),
    }
}

fn kill_process(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: process_id.to_owned(),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill python process through wire");

    match result.response.payload {
        ResponsePayload::ProcessKilledResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire process-killed response: {other:?}"),
    }
}

fn wait_for_stdout_chunk(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    needle: &str,
) {
    let ownership = wire_vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for python stdout containing {needle:?}"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll python stdout through wire");
        let Some(event) = event else { continue };

        match event.payload {
            EventPayload::ProcessOutputEvent(output)
                if output.process_id == process_id
                    && output.channel == StreamChannel::Stdout
                    && chunk_contains(&output.chunk, needle) =>
            {
                return;
            }
            EventPayload::ProcessOutputEvent(_) => {}
            EventPayload::ProcessExitedEvent(exited) if exited.process_id == process_id => {
                panic!(
                    "python process exited before emitting {needle:?}: {:?}",
                    exited.exit_code
                );
            }
            EventPayload::ProcessExitedEvent(_)
            | EventPayload::VmLifecycleEvent(_)
            | EventPayload::StructuredEvent(_)
            | EventPayload::ExtEnvelope(_) => {}
        }
    }
}

fn python_runtime_executes_code_end_to_end() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-execute");
    let cwd = temp_dir("python-execute-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );
    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python",
        "print('hello world')",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "hello world\n");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from successful python execution: {stderr}"
    );
}

fn python_runtime_executes_workspace_py_file_by_path() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-file-entrypoint");
    let cwd = temp_dir("python-file-entrypoint-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![
                root_dir("/workspace"),
                root_file(
                    "/workspace/script.py",
                    "print('hello from file')\n",
                    Some(RootFilesystemEntryEncoding::Utf8),
                ),
            ],
        },
    );

    execute_python_entrypoint(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-file",
        "/workspace/script.py",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-file",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "hello from file\n");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from file-based Python execution: {stderr}"
    );
}

fn python_runtime_reports_syntax_errors_over_stderr() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-syntax-error");
    let cwd = temp_dir("python-syntax-error-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-error",
        "print(",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-error",
    );

    assert_eq!(exit_code, 1);
    assert!(
        stdout.is_empty(),
        "unexpected stdout from syntax error execution: {stdout}"
    );
    assert!(
        stderr.contains("SyntaxError"),
        "expected SyntaxError in stderr, got: {stderr}"
    );
}

fn python_runtime_blocks_pyodide_js_escape_hatches() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-security");
    let cwd = temp_dir("python-security-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-security",
        r#"
import json
import js
import pyodide_js

def capture(action):
    try:
        action()
        return {"ok": True}
    except Exception as error:
        return {
            "ok": False,
            "type": type(error).__name__,
            "message": str(error),
            "code": getattr(error, "code", None),
        }

result = {
    "js_process_env": capture(lambda: js.process.env),
    "js_require": capture(lambda: js.require),
    "js_process_exit": capture(lambda: js.process.exit),
    "js_process_kill": capture(lambda: js.process.kill),
    "pyodide_js_eval_code": capture(lambda: pyodide_js.eval_code),
}

print(json.dumps(result))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-security",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from python security execution: {stderr}"
    );

    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("python security stdout line");
    let parsed: Value = serde_json::from_str(json_line).expect("parse python security JSON");
    for key in [
        "js_process_env",
        "js_require",
        "js_process_exit",
        "js_process_kill",
    ] {
        assert_eq!(parsed[key]["ok"], Value::Bool(false));
        assert_eq!(
            parsed[key]["type"],
            Value::String(String::from("RuntimeError"))
        );
        assert_eq!(parsed[key]["code"], Value::Null);
        assert!(parsed[key]["message"]
            .as_str()
            .expect("js hardening message")
            .contains("js is not available"));
    }
    assert_eq!(parsed["pyodide_js_eval_code"]["ok"], Value::Bool(false));
    assert_eq!(
        parsed["pyodide_js_eval_code"]["type"],
        Value::String(String::from("RuntimeError"))
    );
    assert_eq!(parsed["pyodide_js_eval_code"]["code"], Value::Null);
    assert!(parsed["pyodide_js_eval_code"]["message"]
        .as_str()
        .expect("pyodide_js hardening message")
        .contains("pyodide_js is not available"));
}

fn concurrent_python_processes_stay_isolated_across_vms() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-process-isolation");
    let cwd = temp_dir("python-process-isolation-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (slow_vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );
    let (fast_vm_id, _) = create_vm_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &slow_vm_id,
        "proc",
        "print('slow python')",
    );
    execute_inline_python(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &fast_vm_id,
        "proc",
        "print('fast python')",
    );

    let mut results = HashMap::from([
        (slow_vm_id.clone(), ProcessResult::default()),
        (fast_vm_id.clone(), ProcessResult::default()),
    ]);
    let deadline = Instant::now() + Duration::from_secs(15);
    let ownership = wire_session(&connection_id, &session_id);

    while results.values().any(|result| result.exit_code.is_none()) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for concurrent python process events"
        );
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll python process wire event");
        let Some(event) = event else { continue };

        let OwnershipScope::VmOwnership(ownership) = event.ownership else {
            panic!("expected vm-scoped python process event");
        };
        let result = results
            .get_mut(&ownership.vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {}", ownership.vm_id));

        match event.payload {
            EventPayload::ProcessOutputEvent(output) => {
                assert_eq!(output.process_id, "proc");
                match output.channel {
                    StreamChannel::Stdout => {
                        append_stream_chunk(&mut result.stdout, &output.chunk, "stdout");
                    }
                    StreamChannel::Stderr => {
                        append_stream_chunk(&mut result.stderr, &output.chunk, "stderr");
                    }
                }
            }
            EventPayload::ProcessExitedEvent(exited) => {
                assert_eq!(exited.process_id, "proc");
                result.exit_code = Some(exited.exit_code);
            }
            EventPayload::VmLifecycleEvent(_)
            | EventPayload::StructuredEvent(_)
            | EventPayload::ExtEnvelope(_) => {}
        }
    }

    let slow = results.get(&slow_vm_id).expect("slow vm result");
    let fast = results.get(&fast_vm_id).expect("fast vm result");

    assert_eq!(slow.exit_code, Some(0));
    assert_eq!(fast.exit_code, Some(0));
    let slow_stdout = String::from_utf8_lossy(&slow.stdout);
    let fast_stdout = String::from_utf8_lossy(&fast.stdout);
    let slow_stderr = String::from_utf8_lossy(&slow.stderr);
    let fast_stderr = String::from_utf8_lossy(&fast.stderr);
    assert_eq!(slow_stdout, "slow python\n");
    assert_eq!(fast_stdout, "fast python\n");
    assert!(
        slow_stderr.is_empty(),
        "unexpected slow python stderr: {}",
        slow_stderr
    );
    assert!(
        fast_stderr.is_empty(),
        "unexpected fast python stderr: {}",
        fast_stderr
    );
}

fn python_runtime_mounts_workspace_over_the_kernel_vfs() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-workspace-vfs");
    let cwd = temp_dir("python-workspace-vfs-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_dir("/workspace")],
    );
    guest_write_file_utf8(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/from-kernel.txt",
        "from kernel",
    );

    execute_inline_python(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-workspace",
        r#"
import json
import os

with open("/workspace/from-kernel.txt", "r", encoding="utf-8") as handle:
    original = handle.read()

with open("/workspace/from-python.txt", "w", encoding="utf-8") as handle:
    handle.write("from python")

print(json.dumps({
    "original": original,
    "entries": sorted(os.listdir("/workspace")),
}))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-workspace",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from workspace mount execution: {stderr}"
    );

    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse workspace mount JSON");
    assert_eq!(parsed["original"], "from kernel");
    assert_eq!(
        parsed["entries"],
        serde_json::json!(["from-kernel.txt", "from-python.txt"])
    );

    let python_written = guest_read_file_utf8(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/from-python.txt",
    );
    assert_eq!(python_written, "from python");
}

fn python_runtime_supports_file_delete_and_rename() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-file-ops");
    let cwd = temp_dir("python-file-ops-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_dir("/workspace")],
    );
    // Seed via the kernel so delete/rename exercise host-backed VFS entries.
    guest_write_file_utf8(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/seed.txt",
        "seed",
    );

    execute_inline_python(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-file-ops",
        r#"
import json
import os

results = {}

# delete a file
os.remove("/workspace/seed.txt")
results["seed_exists"] = os.path.exists("/workspace/seed.txt")

# create then remove a directory
os.mkdir("/workspace/subdir")
results["subdir_made"] = os.path.isdir("/workspace/subdir")
os.rmdir("/workspace/subdir")
results["subdir_exists"] = os.path.exists("/workspace/subdir")

# rename a file
with open("/workspace/old.txt", "w", encoding="utf-8") as handle:
    handle.write("renamed body")
os.rename("/workspace/old.txt", "/workspace/new.txt")
results["old_exists"] = os.path.exists("/workspace/old.txt")
with open("/workspace/new.txt", "r", encoding="utf-8") as handle:
    results["new_body"] = handle.read()

results["entries"] = sorted(os.listdir("/workspace"))
print(json.dumps(results))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-file-ops",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse file-ops JSON");
    assert_eq!(parsed["seed_exists"], false, "seed.txt should be deleted");
    assert_eq!(parsed["subdir_made"], true);
    assert_eq!(parsed["subdir_exists"], false, "subdir should be removed");
    assert_eq!(
        parsed["old_exists"], false,
        "old.txt should be renamed away"
    );
    assert_eq!(parsed["new_body"], "renamed body");
    assert_eq!(parsed["entries"], serde_json::json!(["new.txt"]));

    // Cross-check the HOST kernel VFS reflects the deletes/rename.
    assert!(
        !guest_exists(
            &mut sidecar,
            7,
            &connection_id,
            &session_id,
            &vm_id,
            "/workspace/seed.txt"
        ),
        "host VFS should not see deleted seed.txt"
    );
    assert!(
        !guest_exists(
            &mut sidecar,
            8,
            &connection_id,
            &session_id,
            &vm_id,
            "/workspace/old.txt"
        ),
        "host VFS should not see renamed-away old.txt"
    );
    assert!(
        !guest_exists(
            &mut sidecar,
            9,
            &connection_id,
            &session_id,
            &vm_id,
            "/workspace/subdir"
        ),
        "host VFS should not see removed subdir"
    );
    let new_body = guest_read_file_utf8(
        &mut sidecar,
        10,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/new.txt",
    );
    assert_eq!(
        new_body, "renamed body",
        "host VFS should see the renamed file"
    );
}

fn python_runtime_supports_raw_tcp_and_udp_sockets() {
    assert_node_available();

    let (tcp_port, tcp_server) = spawn_tcp_echo_server();
    let (udp_port, udp_server) = spawn_udp_echo_server();
    let mut sidecar = new_sidecar("python-sockets");
    let cwd = temp_dir("python-sockets-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![tcp_port.to_string(), udp_port.to_string()])
                .expect("serialize exempt ports"),
        )]),
        wire_permissions_allow_all(),
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-sockets",
        &format!(
            r#"
import json
import socket

result = {{}}

# Raw outbound TCP against a host echo server.
tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
tcp.settimeout(10)
tcp.connect(("127.0.0.1", {tcp_port}))
tcp.sendall(b"hello sockets")
received = b""
while len(received) < len(b"hello sockets"):
    chunk = tcp.recv(64)
    if not chunk:
        break
    received += chunk
tcp.close()
result["tcp"] = received.decode()

# Raw UDP against a host echo server.
udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
udp.settimeout(10)
udp.sendto(b"ping udp", ("127.0.0.1", {udp_port}))
data, _addr = udp.recvfrom(64)
udp.close()
result["udp"] = data.decode()

print(json.dumps(result))
"#,
        ),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-sockets",
        Duration::from_secs(120),
    );

    let _ = tcp_server.join();
    let _ = udp_server.join();
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("python sockets stdout line");
    let parsed: Value = serde_json::from_str(json_line).expect("parse sockets JSON");
    assert_eq!(parsed["tcp"], "hello sockets");
    assert_eq!(parsed["udp"], "ping udp");
}

fn python_runtime_supports_symlink_readlink_and_metadata() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-fs-hooks");
    let cwd = temp_dir("python-fs-hooks-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_dir("/workspace")],
    );

    // A symlink that already exists on the host (created via the wire, not by
    // Python) — exercises lstat-based detection of pre-existing links.
    guest_symlink(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/hostlink.txt",
        "file.txt",
    );

    execute_inline_python(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-fs-hooks",
        r#"
import json
import os

result = {}

with open("/workspace/file.txt", "w", encoding="utf-8") as handle:
    handle.write("data")

# symlink + readlink (created by Python)
os.symlink("file.txt", "/workspace/link.txt")
result["readlink"] = os.readlink("/workspace/link.txt")
result["islink"] = os.path.islink("/workspace/link.txt")

# a symlink that pre-existed on the host is detected as a link
result["host_islink"] = os.path.islink("/workspace/hostlink.txt")
result["host_readlink"] = os.readlink("/workspace/hostlink.txt")

# chmod (setattr -> host)
os.chmod("/workspace/file.txt", 0o640)
result["mode"] = os.stat("/workspace/file.txt").st_mode & 0o777

# utimes (setattr -> host) — just exercise the hook
os.utime("/workspace/file.txt", (1700000000, 1710000000))

print(json.dumps(result))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-fs-hooks",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse fs-hooks JSON");
    assert_eq!(
        parsed["readlink"], "file.txt",
        "os.readlink should return the target"
    );
    assert_eq!(parsed["islink"], true, "os.path.islink should be true");
    assert_eq!(
        parsed["host_islink"], true,
        "a host-preexisting symlink should be detected as a link"
    );
    assert_eq!(parsed["host_readlink"], "file.txt");
    assert_eq!(parsed["mode"], 0o640, "os.chmod should be reflected");

    // Cross-check the host kernel VFS.
    let host_target = guest_readlink(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/link.txt",
    );
    assert_eq!(
        host_target, "file.txt",
        "host VFS should resolve the symlink"
    );
    let host_mode = guest_stat_mode(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "/workspace/file.txt",
    );
    assert_eq!(
        host_mode & 0o777,
        0o640,
        "host VFS should reflect the chmod"
    );
}

fn workspace_files_are_shared_between_javascript_and_python_runtimes() {
    assert_node_available();

    let mut sidecar = new_sidecar("cross-runtime-workspace");
    let workspace_host_dir = temp_dir("cross-runtime-workspace-host");
    let cwd = workspace_host_dir.clone();
    let js_entry = workspace_host_dir.join("cross-runtime.cjs");
    let connection_id = authenticate_wire(&mut sidecar, "conn-cross-runtime");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    write_fixture(
        &js_entry,
        r#"
const fs = require('node:fs');

const mode = process.argv[2];

if (mode === 'write') {
  fs.writeFileSync('/workspace/from-js.txt', Buffer.from('from js'));
  const result = JSON.stringify({
    entries: fs.readdirSync('/workspace').sort(),
  });
  fs.writeFileSync('/workspace/js-write-result.json', Buffer.from(result));
} else if (mode === 'read') {
  const result = JSON.stringify({
    fromPython: fs.readFileSync('/workspace/from-python.txt', 'utf8'),
    entries: fs.readdirSync('/workspace').sort(),
  });
  fs.writeFileSync('/workspace/js-read-result.json', Buffer.from(result));
} else {
  throw new Error(`unknown mode: ${mode}`);
}
"#,
    );

    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_dir("/workspace")],
    );
    let configure = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    guest_source: String::from("host_dir"),
                    guest_fstype: String::from("host_dir"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: json!({
                            "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
                            "readOnly": false,
                        })
                        .to_string(),
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
        .expect("configure host_dir workspace mount through wire");
    match configure.response.payload {
        ResponsePayload::VmConfiguredResponse(response) => {
            // 1 = just the client mount. With no packages configured there are no
            // granular /opt/agentos leaf mounts (one tar/bin/current mount is added
            // per package, not a single always-present staging mount).
            assert_eq!(response.applied_mounts, 1);
        }
        other => panic!("unexpected wire configure-vm response: {other:?}"),
    }

    let js_fs_env = HashMap::from([
        (
            String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
            json!([{
                "guestPath": "/workspace",
                "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
            }])
            .to_string(),
        ),
        (
            String::from("AGENTOS_EXTRA_FS_READ_PATHS"),
            json!([workspace_host_dir.to_string_lossy().into_owned()]).to_string(),
        ),
        (
            String::from("AGENTOS_EXTRA_FS_WRITE_PATHS"),
            json!([workspace_host_dir.to_string_lossy().into_owned()]).to_string(),
        ),
    ]);

    execute_javascript_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-write",
        &js_entry,
        vec![String::from("write")],
        js_fs_env.clone(),
    );
    let (js_write_stdout, js_write_stderr, js_write_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-write",
    );

    assert_eq!(
        js_write_exit, 0,
        "stdout: {js_write_stdout}\nstderr: {js_write_stderr}"
    );
    assert!(
        js_write_stderr.is_empty(),
        "unexpected stderr from JavaScript write execution: {js_write_stderr}"
    );
    let js_write: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_host_dir.join("js-write-result.json"))
            .expect("read JavaScript write JSON"),
    )
    .expect("parse JavaScript write JSON");
    assert_eq!(
        js_write["entries"],
        serde_json::json!(["cross-runtime.cjs", "from-js.txt"])
    );

    execute_inline_python(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-cross-runtime",
        r#"
import json
import os

with open("/workspace/from-js.txt", "r", encoding="utf-8") as handle:
    from_js = handle.read()

with open("/workspace/from-python.txt", "w", encoding="utf-8") as handle:
    handle.write("from python")

print(json.dumps({
    "fromJs": from_js,
    "entries": sorted(os.listdir("/workspace")),
}))
"#,
    );
    let (python_stdout, python_stderr, python_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-cross-runtime",
    );

    assert_eq!(
        python_exit, 0,
        "stdout: {python_stdout}\nstderr: {python_stderr}"
    );
    assert!(
        python_stderr.is_empty(),
        "unexpected stderr from Python cross-runtime execution: {python_stderr}"
    );
    let python_result: Value =
        serde_json::from_str(python_stdout.trim()).expect("parse Python cross-runtime JSON");
    assert_eq!(python_result["fromJs"], "from js");
    assert_eq!(
        python_result["entries"],
        serde_json::json!([
            "cross-runtime.cjs",
            "from-js.txt",
            "from-python.txt",
            "js-write-result.json"
        ])
    );

    execute_javascript_with_env(
        &mut sidecar,
        8,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-read",
        &js_entry,
        vec![String::from("read")],
        js_fs_env,
    );
    let (js_read_stdout, js_read_stderr, js_read_exit) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-read",
    );

    assert_eq!(
        js_read_exit, 0,
        "stdout: {js_read_stdout}\nstderr: {js_read_stderr}"
    );
    assert!(
        js_read_stderr.is_empty(),
        "unexpected stderr from JavaScript read execution: {js_read_stderr}"
    );
    let js_read: Value = serde_json::from_str(
        &std::fs::read_to_string(workspace_host_dir.join("js-read-result.json"))
            .expect("read JavaScript read JSON"),
    )
    .expect("parse JavaScript read JSON");
    assert_eq!(js_read["fromPython"], "from python");
    assert_eq!(
        js_read["entries"],
        serde_json::json!([
            "cross-runtime.cjs",
            "from-js.txt",
            "from-python.txt",
            "js-write-result.json"
        ])
    );
}

fn python_workspace_mount_respects_read_only_root_permissions() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-workspace-readonly");
    let cwd = temp_dir("python-workspace-readonly-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::ReadOnly,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![
                root_dir("/workspace"),
                root_file(
                    "/workspace/existing.txt",
                    "seed",
                    Some(RootFilesystemEntryEncoding::Utf8),
                ),
            ],
        },
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-workspace-readonly",
        r#"
from pathlib import Path

try:
    Path("/workspace/blocked.txt").write_text("blocked", encoding="utf-8")
    print("write-ok")
except Exception as error:
    print(type(error).__name__)
    print(str(error))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-workspace-readonly",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from readonly workspace execution: {stderr}"
    );
    assert!(
        !stdout.contains("write-ok"),
        "python workspace write unexpectedly succeeded: {stdout}"
    );
    assert!(
        stdout.contains("PermissionError") || stdout.contains("OSError"),
        "expected a Python filesystem error, got: {stdout}"
    );
    assert!(
        stdout.to_ascii_lowercase().contains("read-only")
            || stdout.to_ascii_lowercase().contains("permission denied"),
        "expected readonly or permission message, got: {stdout}"
    );
}

fn python_runtime_blocks_mapped_pyodide_cache_symlink_metadata_escape() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-pyodide-cache-symlink-escape");
    let cwd = temp_dir("python-pyodide-cache-symlink-escape-cwd");
    let mapped_cache_root = temp_dir("python-pyodide-cache-symlink-root");
    let outside_root = temp_dir("python-pyodide-cache-symlink-outside");
    let mapped_pkg_dir = mapped_cache_root.join("pkg");
    let outside_secret = outside_root.join("secret.txt");
    fs::create_dir_all(&mapped_pkg_dir).expect("create mapped cache package dir");
    write_fixture(&outside_secret, "outside secret");
    symlink(&outside_secret, mapped_pkg_dir.join("link"))
        .expect("create outside symlink in mapped cache");

    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pyodide-cache-symlink-escape",
        r#"
import json
import os

result = {}

try:
    stat = os.stat("/__agentos_pyodide_cache/pkg/link")
    result["stat"] = {
        "ok": True,
        "size": stat.st_size,
        "dev": stat.st_dev,
        "ino": stat.st_ino,
    }
except OSError as error:
    result["stat"] = {
        "ok": False,
        "errno": error.errno,
        "message": str(error),
    }

try:
    result["entries"] = sorted(os.listdir("/__agentos_pyodide_cache/pkg"))
except OSError as error:
    result["entries"] = []
    result["entriesError"] = {
        "errno": error.errno,
        "message": str(error),
    }
print(json.dumps(result))
"#,
        HashMap::from([(
            String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
            serde_json::to_string(&vec![json!({
                "guestPath": "/__agentos_pyodide_cache",
                "hostPath": mapped_cache_root.to_string_lossy().into_owned(),
            })])
            .expect("serialize mapped cache root"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pyodide-cache-symlink-escape",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from python execution: {stderr}"
    );

    let result_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("python symlink-escape JSON line");
    let parsed: Value =
        serde_json::from_str(result_line).expect("parse python symlink-escape JSON");
    assert_eq!(parsed["stat"]["ok"], Value::Bool(false));
    let errno = parsed["stat"]["errno"]
        .as_i64()
        .expect("symlink-escape errno should be numeric");
    assert!(
        errno == i64::from(libc::ENOENT)
            || errno == i64::from(libc::EPERM)
            || errno == i64::from(libc::EACCES)
            || errno == 44
            || parsed["stat"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("No such file or directory")),
        "expected ENOENT/EPERM/EACCES from escaped symlink stat, got: {parsed}"
    );
    assert_eq!(parsed["entries"], Value::Array(Vec::new()));
    if !parsed["entriesError"].is_null() {
        let entries_errno = parsed["entriesError"]["errno"]
            .as_i64()
            .expect("entries errno should be numeric");
        assert!(
            entries_errno == i64::from(libc::ENOENT)
                || entries_errno == i64::from(libc::EPERM)
                || entries_errno == i64::from(libc::EACCES)
                || entries_errno == 44
                || parsed["entriesError"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("No such file or directory")),
            "expected ENOENT/EPERM/EACCES-style denial from mapped cache listing, got: {parsed}"
        );
    }
}

fn python_runtime_blocks_mapped_pyodide_cache_symlink_swap_toctou_escape() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-pyodide-cache-symlink-swap-race");
    let cwd = temp_dir("python-pyodide-cache-symlink-swap-race-cwd");
    let mapped_cache_root = temp_dir("python-pyodide-cache-symlink-swap-race-root");
    let outside_root = temp_dir("python-pyodide-cache-symlink-swap-race-outside");
    let safe_pkg_dir = mapped_cache_root.join("safe-pkg");
    let pkg_link_path = mapped_cache_root.join("pkg");
    let safe_secret = safe_pkg_dir.join("secret.txt");
    let outside_secret = outside_root.join("secret.txt");
    fs::create_dir_all(&safe_pkg_dir).expect("create mapped safe package dir");
    fs::create_dir_all(&outside_root).expect("create outside package dir");
    write_fixture(&safe_secret, "safe secret");
    write_fixture(&outside_secret, "outside secret");
    symlink(&safe_pkg_dir, &pkg_link_path).expect("create initial safe package symlink");

    let stop = Arc::new(AtomicBool::new(false));
    let flapper_stop = Arc::clone(&stop);
    let flapper_pkg_link_path = pkg_link_path.clone();
    let flapper_safe_pkg_dir = safe_pkg_dir.clone();
    let flapper_outside_root = outside_root.clone();
    let flapper = thread::spawn(move || {
        let mut swap_index = 0usize;
        while !flapper_stop.load(Ordering::Relaxed) {
            let next_target = if swap_index.is_multiple_of(2) {
                &flapper_outside_root
            } else {
                &flapper_safe_pkg_dir
            };
            let temp_link =
                flapper_pkg_link_path.with_file_name(format!(".pkg-swap-{}", swap_index % 2));
            let _ = fs::remove_file(&temp_link);
            symlink(next_target, &temp_link).expect("create swap symlink");
            fs::rename(&temp_link, &flapper_pkg_link_path).expect("swap package symlink");
            swap_index += 1;
        }
    });

    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pyodide-cache-symlink-swap-race",
        r#"
import json

result = {"safe": 0, "outside": 0, "errors": 0, "unexpected": []}
for _ in range(4000):
    try:
        with open("/__agentos_pyodide_cache/pkg/secret.txt", "r", encoding="utf-8") as handle:
            value = handle.read().strip()
        if value == "safe secret":
            result["safe"] += 1
        elif value == "outside secret":
            result["outside"] += 1
        else:
            result["unexpected"].append(value)
    except OSError:
        result["errors"] += 1

print(json.dumps(result))
"#,
        HashMap::from([(
            String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
            serde_json::to_string(&vec![json!({
                "guestPath": "/__agentos_pyodide_cache",
                "hostPath": mapped_cache_root.to_string_lossy().into_owned(),
            })])
            .expect("serialize mapped cache root"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pyodide-cache-symlink-swap-race",
    );
    stop.store(true, Ordering::Relaxed);
    flapper.join().expect("join package symlink flapper");

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from python execution: {stderr}"
    );

    let result_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("python symlink-swap race JSON line");
    let parsed: Value =
        serde_json::from_str(result_line).expect("parse python symlink-swap race JSON");
    assert_eq!(
        parsed["outside"],
        Value::from(0),
        "mapped cache read escaped to outside root during symlink swap race: {parsed}"
    );
    assert_eq!(
        parsed["unexpected"],
        Value::Array(Vec::new()),
        "mapped cache read returned unexpected content during symlink swap race: {parsed}"
    );
    assert!(
        parsed["safe"].as_i64().unwrap_or_default() > 0
            || parsed["errors"].as_i64().unwrap_or_default() > 0,
        "expected safe reads or denied race windows, got: {parsed}"
    );
}

fn python_runtime_routes_stdin_writes_and_close_to_pyodide() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-stdin");
    let cwd = temp_dir("python-stdin-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin",
        r#"
import sys

print("ready")
print(f"input:{input()}")
print(f"read:{sys.stdin.read()!r}")
"#,
    );

    wait_for_stdout_chunk(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin",
        "ready",
    );
    assert!(
        sidecar
            .poll_event_wire_blocking(
                &wire_vm(&connection_id, &session_id, &vm_id),
                Duration::from_millis(200)
            )
            .expect("poll stalled python stdin")
            .is_none(),
        "python stdin execution should wait for input before exiting"
    );

    write_process_stdin(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin",
        "hello\nrest",
    );
    close_process_stdin(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected python stdin stderr: {stderr}"
    );
    assert!(
        stdout.contains("input:hello"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("read:'rest'"),
        "unexpected stdout: {stdout}"
    );
}

fn python_runtime_supports_interactive_input_prompts_and_multiple_streaming_writes() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-stdin-interactive");
    let cwd = temp_dir("python-stdin-interactive-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
        r#"
import sys

first = input("prompt-1: ")
print(f"first:{first}")
second = input("prompt-2: ")
print(f"second:{second}")
print(f"tail:{sys.stdin.read()!r}")
"#,
    );

    assert!(
        sidecar
            .poll_event_wire_blocking(
                &wire_vm(&connection_id, &session_id, &vm_id),
                Duration::from_millis(200)
            )
            .expect("poll stalled python interactive stdin before first write")
            .is_none(),
        "python interactive stdin execution should wait for the first input"
    );

    write_process_stdin(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
        "alpha\n",
    );

    wait_for_stdout_chunk(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
        "first:alpha",
    );

    assert!(
        sidecar
            .poll_event_wire_blocking(
                &wire_vm(&connection_id, &session_id, &vm_id),
                Duration::from_millis(200)
            )
            .expect("poll stalled python interactive stdin before second write")
            .is_none(),
        "python interactive stdin execution should stay blocked for the second input"
    );

    write_process_stdin(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
        "beta\ngamma",
    );
    close_process_stdin(
        &mut sidecar,
        7,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-stdin-interactive",
    );

    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected python interactive stdin stderr: {stderr}"
    );
    assert!(
        stdout.contains("second:beta"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("tail:'gamma'"),
        "unexpected stdout: {stdout}"
    );
}

fn python_runtime_close_stdin_triggers_input_eof_and_empty_read() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-stdin-eof");
    let cwd = temp_dir("python-stdin-eof-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-eof",
        r#"
import sys

try:
    input()
except EOFError:
    print("input-eof")

print(f"read:{sys.stdin.read()!r}")
"#,
    );

    close_process_stdin(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-eof",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-eof",
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected python eof stderr: {stderr}");
    assert!(stdout.contains("input-eof"), "unexpected stdout: {stdout}");
    assert!(stdout.contains("read:''"), "unexpected stdout: {stdout}");
}

fn python_runtime_kill_process_terminates_blocked_stdin_reads() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-kill");
    let cwd = temp_dir("python-kill-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-kill",
        r#"
import sys

print("ready")
sys.stdin.read()
"#,
    );

    wait_for_stdout_chunk(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-kill",
        "ready",
    );

    kill_process(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-kill",
    );

    let (_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-kill",
    );

    assert_ne!(exit_code, 0);
    assert!(
        stderr.is_empty()
            || stderr.contains("terminated")
            || stderr.contains("SIGTERM")
            || stderr.contains("Error: null"),
        "unexpected python kill stderr: {stderr}"
    );
}

fn python_runtime_imports_bundled_numpy_without_network() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-numpy-package");
    let cwd = temp_dir("python-numpy-package-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-numpy",
        "import numpy\nprint(numpy.__version__)",
        HashMap::from([(
            String::from("AGENTOS_PYTHON_PRELOAD_PACKAGES"),
            String::from("[\"numpy\"]"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-numpy",
        Duration::from_secs(30),
    );

    assert_eq!(exit_code, 0);
    assert!(
        stderr.is_empty(),
        "unexpected stderr from bundled numpy import: {stderr}"
    );
    assert!(
        stdout.lines().any(|line| line.trim() == "2.2.5"),
        "expected numpy version in stdout, got: {stdout}"
    );
}

fn python_runtime_imports_bundled_pandas_without_network() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-pandas-package");
    let cwd = temp_dir("python-pandas-package-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pandas",
        "import pandas\nprint(pandas.__version__)",
        HashMap::from([(
            String::from("AGENTOS_PYTHON_PRELOAD_PACKAGES"),
            String::from("[\"pandas\"]"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-pandas",
        Duration::from_secs(30),
    );

    assert_eq!(exit_code, 0);
    assert!(
        stderr.is_empty(),
        "unexpected stderr from bundled pandas import: {stderr}"
    );
    assert!(
        stdout.lines().any(|line| line.trim() == "2.3.3"),
        "expected pandas version in stdout, got: {stdout}"
    );
}

fn python_runtime_supports_micropip_package_installation() {
    assert_node_available();

    let (port, server) = spawn_static_file_server(pyodide_asset_dir());
    let mut sidecar = new_sidecar("python-micropip-install");
    let cwd = temp_dir("python-micropip-install-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        wire_permissions_allow_all(),
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-micropip-install",
        &format!(
            r#"
import json
import micropip

await micropip.install("http://127.0.0.1:{port}/click-8.3.1-py3-none-any.whl")

import click
print(json.dumps({{
    "version": click.__version__,
    "command_name": click.Command("demo").name,
}}))
"#,
        ),
        HashMap::from([(
            String::from("AGENTOS_PYODIDE_PACKAGE_BASE_URL"),
            format!("http://127.0.0.1:{port}/"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-micropip-install",
        Duration::from_secs(90),
    );

    let _ = server.join();
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let json_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("micropip stdout line");
    let parsed: Value = serde_json::from_str(json_line).expect("parse micropip JSON");
    assert_eq!(parsed["version"], Value::String(String::from("8.3.1")));
    assert_eq!(parsed["command_name"], Value::String(String::from("demo")));
}

fn python_runtime_micropip_install_respects_network_permissions() {
    assert_node_available();

    let (port, server) = spawn_static_file_server(pyodide_asset_dir());
    let mut sidecar = new_sidecar("python-micropip-network-denied");
    let cwd = temp_dir("python-micropip-network-denied-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        PermissionsPolicy {
            fs: wire_permissions_allow_all().fs,
            network: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            child_process: wire_permissions_allow_all().child_process,
            process: wire_permissions_allow_all().process,
            env: wire_permissions_allow_all().env,
            binding: wire_permissions_allow_all().binding,
        },
    );

    execute_inline_python_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-micropip-network-denied",
        &format!(
            r#"
import micropip
await micropip.install("http://127.0.0.1:{port}/click-8.3.1-py3-none-any.whl")
"#,
        ),
        HashMap::from([(
            String::from("AGENTOS_PYODIDE_PACKAGE_BASE_URL"),
            format!("http://127.0.0.1:{port}/"),
        )]),
    );

    let (_stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-micropip-network-denied",
        Duration::from_secs(30),
    );

    let _ = server.join();
    assert_ne!(exit_code, 0);
    assert!(
        stderr.contains("permission") || stderr.contains("denied") || stderr.contains("EACCES"),
        "expected micropip permission error, got: {stderr}"
    );
}

fn python_runtime_routes_dns_and_http_through_sidecar_bridge() {
    assert_node_available();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept http client");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).expect("read http request");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nhello world",
            )
            .expect("write http response");
    });

    let mut sidecar = new_sidecar("python-network-bridge");
    let cwd = temp_dir("python-network-bridge-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([
            (
                String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
                serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
            ),
            (
                String::from("network.dns.override.example.test"),
                String::from("127.0.0.1"),
            ),
        ]),
        wire_permissions_allow_all(),
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-network",
        &format!(
            r#"
import json
import socket
import urllib.request

lookup = socket.getaddrinfo("example.test", {port}, family=socket.AF_INET, type=socket.SOCK_STREAM)
with urllib.request.urlopen("http://example.test:{port}/urllib") as response:
    urllib_status = response.status
    urllib_body = response.read().decode("utf-8")

print(json.dumps({{
    "lookup": [entry[4][0] for entry in lookup],
    "urllib": {{"status": urllib_status, "body": urllib_body}},
}}))
"#,
        ),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-network",
        Duration::from_secs(30),
    );

    let _ = server;
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse python network JSON");
    assert_eq!(
        parsed["lookup"][0],
        Value::String(String::from("127.0.0.1"))
    );
    assert_eq!(parsed["urllib"]["status"], Value::from(200));
    assert_eq!(
        parsed["urllib"]["body"],
        Value::String(String::from("hello world"))
    );
}

fn python_runtime_routes_requests_through_sidecar_bridge() {
    assert_node_available();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind requests listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept requests client");
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).expect("read requests payload");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\nhello world",
            )
            .expect("write requests response");
    });

    let mut sidecar = new_sidecar("python-requests-bridge");
    let cwd = temp_dir("python-requests-bridge-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([
            (
                String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
                serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
            ),
            (
                String::from("network.dns.override.example.test"),
                String::from("127.0.0.1"),
            ),
        ]),
        wire_permissions_allow_all(),
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-requests",
        &format!(
            r#"
import json
import requests

response = requests.get("http://example.test:{port}/requests")
print(json.dumps({{
    "status": response.status_code,
    "body": response.text,
}}))
"#,
        ),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-requests",
        Duration::from_secs(30),
    );

    let _ = server;
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse requests JSON");
    assert_eq!(parsed["status"], Value::from(200));
    assert_eq!(parsed["body"], Value::String(String::from("hello world")));
}

fn python_runtime_surfaces_network_permission_errors() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-network-denied");
    let cwd = temp_dir("python-network-denied-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("network.dns.override.example.test"),
            String::from("127.0.0.1"),
        )]),
        PermissionsPolicy {
            fs: wire_permissions_allow_all().fs,
            network: Some(PatternPermissionScope::PermissionMode(PermissionMode::Deny)),
            child_process: wire_permissions_allow_all().child_process,
            process: wire_permissions_allow_all().process,
            env: wire_permissions_allow_all().env,
            binding: wire_permissions_allow_all().binding,
        },
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-network-denied",
        r#"
import json
import socket
import urllib.request

result = {}
for name, operation in {
    "dns": lambda: socket.getaddrinfo("example.test", 80),
    "http": lambda: urllib.request.urlopen("http://example.test:80/"),
}.items():
    try:
        operation()
        result[name] = {"unexpected": True}
    except Exception as error:
        result[name] = {"type": type(error).__name__, "message": str(error)}

print(json.dumps(result))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-network-denied",
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("parse python network denied JSON");
    assert_eq!(
        parsed["dns"]["type"],
        Value::String(String::from("PermissionError"))
    );
    assert!(
        parsed["dns"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("permission denied")),
        "stdout: {stdout}"
    );
    assert_eq!(
        parsed["http"]["type"],
        Value::String(String::from("PermissionError")),
        "stdout: {stdout}"
    );
}

fn python_runtime_runs_node_subprocesses_through_sidecar_bridge() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-subprocess-bridge");
    let cwd = temp_dir("python-subprocess-bridge-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );
    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_file(
            "/child.mjs",
            "console.log('child-ready')\n",
            None,
        )],
    );

    execute_inline_python(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-subprocess",
        r#"
import json
import subprocess

result = subprocess.run(["node", "./child.mjs"], capture_output=True, text=True, check=True)
print(json.dumps({
    "code": result.returncode,
    "stdout": result.stdout.strip(),
    "stderr": result.stderr.strip(),
}))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-subprocess",
        Duration::from_secs(30),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse python subprocess JSON");
    assert_eq!(parsed["code"], Value::from(0));
    assert_eq!(parsed["stdout"], Value::String(String::from("child-ready")));
    assert_eq!(parsed["stderr"], Value::String(String::new()));
}

fn python_runtime_surfaces_subprocess_permission_errors() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-subprocess-denied");
    let cwd = temp_dir("python-subprocess-denied-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::new(),
        PermissionsPolicy {
            fs: wire_permissions_allow_all().fs,
            network: wire_permissions_allow_all().network,
            child_process: Some(PatternPermissionScope::PatternPermissionRuleSet(
                PatternPermissionRuleSet {
                    default: Some(PermissionMode::Allow),
                    rules: vec![PatternPermissionRule {
                        mode: PermissionMode::Deny,
                        operations: vec![String::from("*")],
                        patterns: vec![String::from("node")],
                    }],
                },
            )),
            process: wire_permissions_allow_all().process,
            env: wire_permissions_allow_all().env,
            binding: wire_permissions_allow_all().binding,
        },
    );

    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-subprocess-denied",
        r#"
import json
import subprocess

try:
    subprocess.run(["node", "--version"], capture_output=True, text=True, check=True)
    result = {"unexpected": True}
except Exception as error:
    result = {"type": type(error).__name__, "message": str(error)}

print(json.dumps(result))
"#,
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-subprocess-denied",
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    let parsed: Value =
        serde_json::from_str(stdout.trim()).expect("parse python subprocess denied JSON");
    assert_eq!(
        parsed["type"],
        Value::String(String::from("PermissionError"))
    );
    assert!(
        parsed["message"]
            .as_str()
            .is_some_and(|message| message.contains("permission denied")),
        "stdout: {stdout}"
    );
}

#[allow(clippy::too_many_arguments)]
fn execute_python_cli(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    command: &str,
    args: &[&str],
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: Some(command.to_owned()),
                runtime: None,
                entrypoint: None,
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: None,
            }),
        ))
        .expect("start python CLI execution through wire");

    match result.response.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire execute response: {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_python_cli_with_env(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    command: &str,
    args: &[&str],
    env: HashMap<String, String>,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: Some(command.to_owned()),
                runtime: None,
                entrypoint: None,
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                env,
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: None,
            }),
        ))
        .expect("start python CLI execution through wire");

    match result.response.payload {
        ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire execute response: {other:?}"),
    }
}

fn python_command_pip_installs_via_micropip() {
    assert_node_available();

    let (port, server) = spawn_static_file_server(pyodide_asset_dir());
    let mut sidecar = new_sidecar("python-cli-pip");
    let cwd = temp_dir("python-cli-pip-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        wire_permissions_allow_all(),
    );

    execute_python_cli_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-pip",
        "pip",
        &[
            "install",
            &format!("http://127.0.0.1:{port}/click-8.3.1-py3-none-any.whl"),
        ],
        HashMap::from([(
            String::from("AGENTOS_PYODIDE_PACKAGE_BASE_URL"),
            format!("http://127.0.0.1:{port}/"),
        )]),
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-pip",
        Duration::from_secs(90),
    );
    let _ = server.join();
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("Successfully installed"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

fn python_command_runs_inline_code() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-inline");
    let cwd = temp_dir("python-cli-inline-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_python_cli(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-c",
        "python",
        &["-c", "print(1 + 1)"],
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-c",
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "2\n", "stderr: {stderr}");
}

fn python_command_runs_script_with_argv() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-argv");
    let cwd = temp_dir("python-cli-argv-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![
                root_dir("/workspace"),
                root_file(
                    "/workspace/argv.py",
                    "import sys\nprint(\",\".join(sys.argv))\n",
                    Some(RootFilesystemEntryEncoding::Utf8),
                ),
            ],
        },
    );

    execute_python_cli(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-argv",
        "python",
        &["/workspace/argv.py", "alpha", "beta"],
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-argv",
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(
        stdout, "/workspace/argv.py,alpha,beta\n",
        "stderr: {stderr}"
    );
}

fn python_command_runs_module_with_dash_m() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-module");
    let cwd = temp_dir("python-cli-module-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_python_cli(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-m",
        "python",
        &["-m", "this"],
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-m",
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    // `python -m this` runs the stdlib `this` module as __main__, printing the Zen.
    assert!(
        stdout.contains("Beautiful is better than ugly"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

fn python_command_reads_program_from_stdin() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-stdin");
    let cwd = temp_dir("python-cli-stdin-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_python_cli(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-stdin",
        "python",
        &["-"],
    );
    write_process_stdin(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-stdin",
        "print('from stdin program')\n",
    );
    close_process_stdin(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-stdin",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-stdin",
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "from stdin program\n", "stderr: {stderr}");
}

fn python_command_runs_interactive_repl() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-repl");
    let cwd = temp_dir("python-cli-repl-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );

    execute_python_cli(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-repl",
        "python",
        &[],
    );
    write_process_stdin(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-repl",
        "print(6 * 7)\n",
    );
    close_process_stdin(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-repl",
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-py-repl",
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("42"), "stdout: {stdout}\nstderr: {stderr}");
}

fn python_command_runs_as_nested_child_process() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-cli-nested");
    let workspace_host_dir = temp_dir("python-cli-nested-host");
    let cwd = workspace_host_dir.clone();
    let js_entry = workspace_host_dir.join("spawn.cjs");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python-nested");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    write_fixture(
        &js_entry,
        r#"
const { spawnSync } = require('node:child_process');
const result = spawnSync('python', ['-c', 'print(2 + 3)'], { encoding: 'utf8' });
if (result.error) {
  process.stderr.write(String(result.error));
}
process.stdout.write('status=' + result.status + ';out=' + (result.stdout || '').trim());
"#,
    );

    bootstrap_root_filesystem(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        vec![root_dir("/workspace")],
    );
    let configure = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    guest_source: String::from("host_dir"),
                    guest_fstype: String::from("host_dir"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: json!({
                            "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
                            "readOnly": false,
                        })
                        .to_string(),
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
        .expect("configure host_dir workspace mount through wire");
    match configure.response.payload {
        ResponsePayload::VmConfiguredResponse(response) => {
            // 1 = just the client mount. With no packages configured there are no
            // granular /opt/agentos leaf mounts (one tar/bin/current mount is added
            // per package, not a single always-present staging mount).
            assert_eq!(response.applied_mounts, 1);
        }
        other => panic!("unexpected wire configure-vm response: {other:?}"),
    }

    let js_fs_env = HashMap::from([
        (
            String::from("AGENTOS_GUEST_PATH_MAPPINGS"),
            json!([{
                "guestPath": "/workspace",
                "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
            }])
            .to_string(),
        ),
        (
            String::from("AGENTOS_EXTRA_FS_READ_PATHS"),
            json!([workspace_host_dir.to_string_lossy().into_owned()]).to_string(),
        ),
    ]);

    execute_javascript_with_env(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-spawn",
        &js_entry,
        Vec::new(),
        js_fs_env,
    );

    let (stdout, stderr, exit_code) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-spawn",
        Duration::from_secs(60),
    );
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert!(
        stdout.contains("status=0;out=5"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
}

fn python_reads_and_writes_arbitrary_vm_paths() {
    assert_node_available();
    let mut sidecar = new_sidecar("python-rootfs-rw");
    let cwd = temp_dir("python-rootfs-rw-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![root_file(
                "/etc/agentos-test.txt",
                "hello-from-etc\n",
                Some(RootFilesystemEntryEncoding::Utf8),
            )],
        },
    );

    // Read from /etc and write to /tmp — both outside the old /workspace window.
    execute_inline_python(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-rw",
        "data = open('/etc/agentos-test.txt').read()\nwith open('/tmp/py-out.txt', 'w') as handle:\n    handle.write('written-by-python\\n')\nprint(data.strip())\n",
    );
    let (stdout, stderr, exit_code) =
        collect_process_output(&mut sidecar, &connection_id, &session_id, &vm_id, "proc-rw");
    assert_eq!(exit_code, 0, "stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(stdout, "hello-from-etc\n", "stderr: {stderr}");

    // A SEPARATE Python process (fresh Pyodide FS) sees /tmp/py-out.txt — proving
    // the write landed in the kernel VFS, not the per-process in-memory FS.
    execute_inline_python(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-reread",
        "print(open('/tmp/py-out.txt').read().strip())\n",
    );
    let (stdout2, stderr2, exit2) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-reread",
    );
    assert_eq!(exit2, 0, "stdout: {stdout2}\nstderr: {stderr2}");
    assert_eq!(stdout2, "written-by-python\n", "stderr: {stderr2}");
}

fn python_pip_installs_persist_across_invocations() {
    assert_node_available();
    let (port, server) = spawn_static_file_server(pyodide_asset_dir());
    let mut sidecar = new_sidecar("python-vfs-pip");
    let cwd = temp_dir("python-vfs-pip-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-python");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        HashMap::from([(
            String::from("env.AGENTOS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        wire_permissions_allow_all(),
    );

    // Process 1: `pip install` a wheel — the shim copies it into the VFS-backed
    // site-packages so it persists past this interpreter.
    execute_python_cli_with_env(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-pip-install",
        "pip",
        &[
            "install",
            &format!("http://127.0.0.1:{port}/click-8.3.1-py3-none-any.whl"),
        ],
        HashMap::from([(
            String::from("AGENTOS_PYODIDE_PACKAGE_BASE_URL"),
            format!("http://127.0.0.1:{port}/"),
        )]),
    );
    let (stdout1, stderr1, exit1) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-pip-install",
        Duration::from_secs(90),
    );
    assert_eq!(exit1, 0, "stdout: {stdout1}\nstderr: {stderr1}");
    assert!(
        stdout1.contains("Successfully installed"),
        "stdout: {stdout1}\nstderr: {stderr1}"
    );
    let home_install_exists = guest_exists(
        &mut sidecar,
        50,
        &connection_id,
        &session_id,
        &vm_id,
        "/home/agentos/.agentos/site-packages/click/__init__.py",
    );
    assert!(
        home_install_exists,
        "pip must persist the installed module under the kernel-owned guest home directory"
    );

    // Process 2: a FRESH Python interpreter imports the package from the VFS
    // site-packages — proving the install persisted across invocations.
    execute_python_cli(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-pip-import",
        "python",
        &["-c", "import click; print(click.__version__)"],
    );
    let (stdout2, stderr2, exit2) = collect_process_output_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-pip-import",
        Duration::from_secs(60),
    );
    let _ = server.join();
    assert_eq!(exit2, 0, "stdout: {stdout2}\nstderr: {stderr2}");
    assert_eq!(
        stdout2.trim(),
        "8.3.1",
        "stdout: {stdout2}\nstderr: {stderr2}"
    );
}

fn python_rootfs_suite() {
    python_reads_and_writes_arbitrary_vm_paths();
    python_pip_installs_persist_across_invocations();
}

fn python_cli_suite() {
    python_command_runs_inline_code();
    python_command_runs_script_with_argv();
    python_command_runs_module_with_dash_m();
    python_command_reads_program_from_stdin();
    python_command_runs_interactive_repl();
    python_command_runs_as_nested_child_process();
    python_command_pip_installs_via_micropip();
}

#[test]
fn python_suite() {
    // Multiple libtest cases in this V8/Pyodide-backed integration binary still
    // trip shared-process teardown/init crashes, so under plain `cargo test`
    // (one process for the whole binary) keep the coverage in one suite.
    //
    // cargo-nextest runs every `#[test]` in its OWN process, so that crash
    // cannot occur there — `mod python_split` below exposes each case as a
    // separate test that nextest runs in parallel across cores (this suite was
    // the single longest test in CI at ~400s serial). Skip the collapsed run
    // under nextest so the work isn't done twice; the split cases skip
    // themselves under `cargo test`. nextest sets `NEXTEST=1` in each test
    // process; libtest/`cargo test` does not.
    if std::env::var_os("NEXTEST").is_some() {
        return;
    }
    static_file_server_rejects_traversal_paths();
    python_runtime_executes_code_end_to_end();
    python_runtime_executes_workspace_py_file_by_path();
    python_runtime_reports_syntax_errors_over_stderr();
    python_runtime_blocks_pyodide_js_escape_hatches();
    concurrent_python_processes_stay_isolated_across_vms();
    python_runtime_mounts_workspace_over_the_kernel_vfs();
    python_runtime_supports_file_delete_and_rename();
    python_runtime_supports_raw_tcp_and_udp_sockets();
    python_runtime_supports_symlink_readlink_and_metadata();
    workspace_files_are_shared_between_javascript_and_python_runtimes();
    python_workspace_mount_respects_read_only_root_permissions();
    python_runtime_blocks_mapped_pyodide_cache_symlink_metadata_escape();
    python_runtime_blocks_mapped_pyodide_cache_symlink_swap_toctou_escape();
    python_runtime_routes_stdin_writes_and_close_to_pyodide();
    python_runtime_supports_interactive_input_prompts_and_multiple_streaming_writes();
    python_runtime_close_stdin_triggers_input_eof_and_empty_read();
    python_runtime_kill_process_terminates_blocked_stdin_reads();
    python_runtime_imports_bundled_numpy_without_network();
    python_runtime_imports_bundled_pandas_without_network();
    python_runtime_supports_micropip_package_installation();
    python_runtime_micropip_install_respects_network_permissions();
    python_runtime_routes_dns_and_http_through_sidecar_bridge();
    python_runtime_routes_requests_through_sidecar_bridge();
    python_runtime_surfaces_network_permission_errors();
    python_runtime_runs_node_subprocesses_through_sidecar_bridge();
    python_runtime_surfaces_subprocess_permission_errors();
    python_cli_suite();
    python_rootfs_suite();
}

/// Per-case split of `python_suite` for cargo-nextest (process-per-test).
///
/// Each `#[test]` here runs exactly one Pyodide case in its own process, so the
/// shared-process V8/Pyodide teardown/init crash that forces the collapsed
/// `python_suite` above does not apply, and the ~36 cases run in parallel across
/// cores instead of serially. Each case skips itself under plain `cargo test`
/// (where `NEXTEST` is unset) so the collapsed suite owns the run there; the
/// collapsed suite likewise skips under nextest so the work isn't duplicated.
mod python_split {
    macro_rules! nextest_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[test]
                fn $case() {
                    // Covered by the collapsed `super::python_suite` under `cargo test`.
                    if std::env::var_os("NEXTEST").is_none() {
                        return;
                    }
                    super::$case();
                }
            )+
        };
    }

    nextest_cases!(
        static_file_server_rejects_traversal_paths,
        python_runtime_executes_code_end_to_end,
        python_runtime_executes_workspace_py_file_by_path,
        python_runtime_reports_syntax_errors_over_stderr,
        python_runtime_blocks_pyodide_js_escape_hatches,
        concurrent_python_processes_stay_isolated_across_vms,
        python_runtime_mounts_workspace_over_the_kernel_vfs,
        python_runtime_supports_file_delete_and_rename,
        python_runtime_supports_raw_tcp_and_udp_sockets,
        python_runtime_supports_symlink_readlink_and_metadata,
        workspace_files_are_shared_between_javascript_and_python_runtimes,
        python_workspace_mount_respects_read_only_root_permissions,
        python_runtime_blocks_mapped_pyodide_cache_symlink_metadata_escape,
        python_runtime_blocks_mapped_pyodide_cache_symlink_swap_toctou_escape,
        python_runtime_routes_stdin_writes_and_close_to_pyodide,
        python_runtime_supports_interactive_input_prompts_and_multiple_streaming_writes,
        python_runtime_close_stdin_triggers_input_eof_and_empty_read,
        python_runtime_kill_process_terminates_blocked_stdin_reads,
        python_runtime_imports_bundled_numpy_without_network,
        python_runtime_imports_bundled_pandas_without_network,
        python_runtime_routes_dns_and_http_through_sidecar_bridge,
        python_runtime_routes_requests_through_sidecar_bridge,
        python_runtime_surfaces_network_permission_errors,
        python_runtime_runs_node_subprocesses_through_sidecar_bridge,
        python_runtime_surfaces_subprocess_permission_errors,
        python_command_runs_inline_code,
        python_command_runs_script_with_argv,
        python_command_runs_module_with_dash_m,
        python_command_reads_program_from_stdin,
        python_command_runs_interactive_repl,
        python_command_runs_as_nested_child_process,
        python_command_pip_installs_via_micropip,
        python_reads_and_writes_arbitrary_vm_paths,
        python_pip_installs_persist_across_invocations,
    );

    // The network-DENIED micropip case can't load the micropip package on its
    // own (network is denied, so the fetch hangs); it relies on micropip already
    // being loaded in the same *process* by a prior network-ALLOWED install
    // (Pyodide's module state is process-global). Under nextest's
    // process-per-test model that shared state is gone, so pair it in one
    // sequential case with a network-allowed install that loads micropip first.
    // The other micropip/pip installs are self-contained (they load micropip
    // themselves) and stay split for parallelism. Skips under `cargo test`,
    // where the collapsed `super::python_suite` owns the run.
    // Named `*_suite` so it inherits nextest.toml's 600s slow-timeout override
    // for `test(/python.*_suite/)` when the denied case runs (nightly).
    #[test]
    fn python_micropip_suite() {
        if std::env::var_os("NEXTEST").is_none() {
            return;
        }
        // Both micropip installs are heavy COLD work under process-per-test (no
        // warm micropip load to reuse): the network-allowed install is ~24s, and
        // the network-denied variant is timeout-bound (~2min on the denied-op
        // timeout). Neither is on the critical path, so run the whole group only in
        // the nightly timing lane (AGENTOS_RUN_TIMING_TESTS=1); `cargo test`'s
        // collapsed `super::python_suite` still covers both locally. The denied
        // case also needs the allowed install loaded in the SAME process, so they
        // stay grouped. See CLAUDE.md > Testing.
        if std::env::var_os("AGENTOS_RUN_TIMING_TESTS").is_none() {
            return;
        }
        super::python_runtime_supports_micropip_package_installation();
        super::python_runtime_micropip_install_respects_network_permissions();
    }
}
