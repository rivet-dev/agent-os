mod support;

use agent_os_sidecar::protocol::{
    BootstrapRootFilesystemRequest, CloseStdinRequest, ConfigureVmRequest, CreateVmRequest,
    EventPayload, ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestRuntimeKind, KillProcessRequest, MountDescriptor, MountPluginDescriptor, OwnershipScope,
    PatternPermissionScope, PermissionMode, PermissionsPolicy, RequestId, RequestPayload,
    ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind, RootFilesystemMode, StreamChannel, WriteStdinRequest,
};
use nix::libc;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, collect_process_output,
    collect_process_output_with_timeout, create_vm, new_sidecar, open_session, temp_dir,
    write_fixture,
};

#[derive(Debug, Default)]
struct ProcessResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}

fn pyodide_asset_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sidecar crate parent")
        .join("execution")
        .join("assets")
        .join("pyodide")
}

fn spawn_static_file_server(root: PathBuf) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind static file listener");
    listener
        .set_nonblocking(true)
        .expect("set nonblocking listener");
    let port = listener.local_addr().expect("listener address").port();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut served_any = false;
        let mut idle_since: Option<Instant> = None;
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
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
                    let relative = path.trim_start_matches('/');
                    let file_path = root.join(relative);
                    let (status_line, body) = match fs::read(&file_path) {
                        Ok(body) => ("HTTP/1.1 200 OK", body),
                        Err(_) => ("HTTP/1.1 404 Not Found", b"missing".to_vec()),
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
                            Some(start) if start.elapsed() >= Duration::from_secs(5) => break,
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
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
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
        BTreeMap::new(),
    );
}

fn execute_inline_python_with_env(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    code: &str,
    env: BTreeMap<String, String>,
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
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
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
        BTreeMap::new(),
    );
}

fn execute_python_entrypoint_with_env(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &str,
    env: BTreeMap<String, String>,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: None,
                runtime: Some(GuestRuntimeKind::Python),
                entrypoint: Some(entrypoint.to_owned()),
                args: Vec::new(),
                env,
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("start python execution");

    match result.response.payload {
        ResponsePayload::ProcessStarted(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected execute response: {other:?}"),
    }
}

fn execute_javascript_with_env(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &Path,
    args: Vec<String>,
    env: BTreeMap<String, String>,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args,
                env,
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("start JavaScript execution");

    match result.response.payload {
        ResponsePayload::ProcessStarted(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected execute response: {other:?}"),
    }
}

fn create_vm_with_root_filesystem(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    root_filesystem: RootFilesystemDescriptor,
) -> String {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem,
                permissions: Some(PermissionsPolicy::allow_all()),
            }),
        ))
        .expect("create sidecar VM");

    match result.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected vm create response: {other:?}"),
    }
}

fn create_vm_with_metadata_and_permissions(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    mut metadata: BTreeMap<String, String>,
    permissions: PermissionsPolicy,
) -> String {
    metadata
        .entry(String::from("cwd"))
        .or_insert_with(|| cwd.to_string_lossy().into_owned());

    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime,
                metadata,
                root_filesystem: Default::default(),
                permissions: Some(permissions),
            }),
        ))
        .expect("create sidecar VM");

    match result.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected vm create response: {other:?}"),
    }
}

fn bootstrap_root_filesystem(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    entries: Vec<RootFilesystemEntry>,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::BootstrapRootFilesystem(BootstrapRootFilesystemRequest { entries }),
        ))
        .expect("bootstrap root filesystem");

    match result.response.payload {
        ResponsePayload::RootFilesystemBootstrapped(response) => {
            assert!(response.entry_count > 0);
        }
        other => panic!("unexpected bootstrap response: {other:?}"),
    }
}

fn guest_filesystem_call(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    payload: GuestFilesystemCallRequest,
) -> agent_os_sidecar::protocol::GuestFilesystemResultResponse {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::GuestFilesystemCall(payload),
        ))
        .expect("guest filesystem call");

    match result.response.payload {
        ResponsePayload::GuestFilesystemResult(response) => response,
        other => panic!("unexpected guest filesystem response: {other:?}"),
    }
}

fn guest_write_file_utf8(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
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
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::WriteFile);
    assert_eq!(response.path, path);
}

fn guest_read_file_utf8(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
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
            mode: None,
            uid: None,
            gid: None,
            atime_ms: None,
            mtime_ms: None,
            len: None,
        },
    );

    assert_eq!(response.operation, GuestFilesystemOperation::ReadFile);
    assert_eq!(response.path, path);
    assert_eq!(response.encoding, Some(RootFilesystemEntryEncoding::Utf8));
    response.content.expect("guest filesystem read content")
}

fn write_process_stdin(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    chunk: &str,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::WriteStdin(WriteStdinRequest {
                process_id: process_id.to_owned(),
                chunk: chunk.to_owned(),
            }),
        ))
        .expect("write python stdin");

    match result.response.payload {
        ResponsePayload::StdinWritten(response) => {
            assert_eq!(response.process_id, process_id);
            assert_eq!(response.accepted_bytes, chunk.len() as u64);
        }
        other => panic!("unexpected stdin-written response: {other:?}"),
    }
}

fn close_process_stdin(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::CloseStdin(CloseStdinRequest {
                process_id: process_id.to_owned(),
            }),
        ))
        .expect("close python stdin");

    match result.response.payload {
        ResponsePayload::StdinClosed(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected stdin-closed response: {other:?}"),
    }
}

fn kill_process(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch_blocking(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::KillProcess(KillProcessRequest {
                process_id: process_id.to_owned(),
                signal: String::from("SIGTERM"),
            }),
        ))
        .expect("kill python process");

    match result.response.payload {
        ResponsePayload::ProcessKilled(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected process-killed response: {other:?}"),
    }
}

fn wait_for_stdout_chunk(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    needle: &str,
) {
    let ownership = OwnershipScope::vm(connection_id, session_id, vm_id);
    let deadline = Instant::now() + Duration::from_secs(10);

    loop {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll python stdout");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for python stdout containing {needle:?}"
            );
            continue;
        };

        match event.payload {
            EventPayload::ProcessOutput(output)
                if output.process_id == process_id
                    && output.channel == StreamChannel::Stdout
                    && output.chunk.contains(needle) =>
            {
                return;
            }
            EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                panic!(
                    "python process exited before emitting {needle:?}: {:?}",
                    exited.exit_code
                );
            }
            _ => {}
        }
    }
}

fn python_runtime_executes_code_end_to_end() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-execute");
    let cwd = temp_dir("python-execute-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            bootstrap_entries: vec![
                RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    executable: false,
                    ..Default::default()
                },
                RootFilesystemEntry {
                    path: String::from("/workspace/script.py"),
                    kind: RootFilesystemEntryKind::File,
                    content: Some(String::from("print('hello from file')\n")),
                    encoding: Some(RootFilesystemEntryEncoding::Utf8),
                    executable: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (slow_vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
    );
    let (fast_vm_id, _) = create_vm(
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

    let mut results = BTreeMap::from([
        (slow_vm_id.clone(), ProcessResult::default()),
        (fast_vm_id.clone(), ProcessResult::default()),
    ]);
    let deadline = Instant::now() + Duration::from_secs(15);
    let ownership = OwnershipScope::session(&connection_id, &session_id);

    while results.values().any(|result| result.exit_code.is_none()) {
        let event = sidecar
            .poll_event_blocking(&ownership, Duration::from_millis(100))
            .expect("poll python process event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for concurrent python process events"
            );
            continue;
        };

        let OwnershipScope::Vm { vm_id, .. } = event.ownership else {
            panic!("expected vm-scoped python process event");
        };
        let result = results
            .get_mut(&vm_id)
            .unwrap_or_else(|| panic!("unexpected vm event for {vm_id}"));

        match event.payload {
            EventPayload::ProcessOutput(output) => match output.channel {
                StreamChannel::Stdout => result.stdout.push_str(&output.chunk),
                StreamChannel::Stderr => result.stderr.push_str(&output.chunk),
            },
            EventPayload::ProcessExited(exited) => {
                assert_eq!(exited.process_id, "proc");
                result.exit_code = Some(exited.exit_code);
            }
            _ => {}
        }
    }

    let slow = results.get(&slow_vm_id).expect("slow vm result");
    let fast = results.get(&fast_vm_id).expect("fast vm result");

    assert_eq!(slow.exit_code, Some(0));
    assert_eq!(fast.exit_code, Some(0));
    assert_eq!(slow.stdout, "slow python\n");
    assert_eq!(fast.stdout, "fast python\n");
    assert!(
        slow.stderr.is_empty(),
        "unexpected slow python stderr: {}",
        slow.stderr
    );
    assert!(
        fast.stderr.is_empty(),
        "unexpected fast python stderr: {}",
        fast.stderr
    );
}

fn python_runtime_mounts_workspace_over_the_kernel_vfs() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-workspace-vfs");
    let cwd = temp_dir("python-workspace-vfs-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
        vec![RootFilesystemEntry {
            path: String::from("/workspace"),
            kind: RootFilesystemEntryKind::Directory,
            executable: false,
            ..Default::default()
        }],
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

fn workspace_files_are_shared_between_javascript_and_python_runtimes() {
    assert_node_available();

    let mut sidecar = new_sidecar("cross-runtime-workspace");
    let workspace_host_dir = temp_dir("cross-runtime-workspace-host");
    let cwd = workspace_host_dir.clone();
    let js_entry = workspace_host_dir.join("cross-runtime.cjs");
    let connection_id = authenticate(&mut sidecar, "conn-cross-runtime");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
        vec![RootFilesystemEntry {
            path: String::from("/workspace"),
            kind: RootFilesystemEntryKind::Directory,
            executable: false,
            ..Default::default()
        }],
    );
    let configure = sidecar
        .dispatch_blocking(support::request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: vec![MountDescriptor {
                    guest_path: String::from("/workspace"),
                    read_only: false,
                    plugin: MountPluginDescriptor {
                        id: String::from("host_dir"),
                        config: json!({
                            "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
                            "readOnly": false,
                        }),
                    },
                }],
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: BTreeMap::new(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("configure host_dir workspace mount");
    match configure.response.payload {
        ResponsePayload::VmConfigured(response) => {
            assert_eq!(response.applied_mounts, 1);
        }
        other => panic!("unexpected configure-vm response: {other:?}"),
    }

    let js_fs_env = BTreeMap::from([
        (
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            json!([{
                "guestPath": "/workspace",
                "hostPath": workspace_host_dir.to_string_lossy().into_owned(),
            }])
            .to_string(),
        ),
        (
            String::from("AGENT_OS_EXTRA_FS_READ_PATHS"),
            json!([workspace_host_dir.to_string_lossy().into_owned()]).to_string(),
        ),
        (
            String::from("AGENT_OS_EXTRA_FS_WRITE_PATHS"),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::ReadOnly,
            bootstrap_entries: vec![
                RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    executable: false,
                    ..Default::default()
                },
                RootFilesystemEntry {
                    path: String::from("/workspace/existing.txt"),
                    kind: RootFilesystemEntryKind::File,
                    content: Some(String::from("seed")),
                    encoding: Some(RootFilesystemEntryEncoding::Utf8),
                    executable: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
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

    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    stat = os.stat("/__agent_os_pyodide_cache/pkg/link")
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
    result["entries"] = sorted(os.listdir("/__agent_os_pyodide_cache/pkg"))
except OSError as error:
    result["entries"] = []
    result["entriesError"] = {
        "errno": error.errno,
        "message": str(error),
    }
print(json.dumps(result))
"#,
        BTreeMap::from([(
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            serde_json::to_string(&vec![json!({
                "guestPath": "/__agent_os_pyodide_cache",
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
            let next_target = if swap_index % 2 == 0 {
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

    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
        with open("/__agent_os_pyodide_cache/pkg/secret.txt", "r", encoding="utf-8") as handle:
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
        BTreeMap::from([(
            String::from("AGENT_OS_GUEST_PATH_MAPPINGS"),
            serde_json::to_string(&vec![json!({
                "guestPath": "/__agent_os_pyodide_cache",
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
            .poll_event_blocking(
                &OwnershipScope::vm(&connection_id, &session_id, &vm_id),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
            .poll_event_blocking(
                &OwnershipScope::vm(&connection_id, &session_id, &vm_id),
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
            .poll_event_blocking(
                &OwnershipScope::vm(&connection_id, &session_id, &vm_id),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
        BTreeMap::from([(
            String::from("AGENT_OS_PYTHON_PRELOAD_PACKAGES"),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
        BTreeMap::from([(
            String::from("AGENT_OS_PYTHON_PRELOAD_PACKAGES"),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::from([(
            String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        PermissionsPolicy::allow_all(),
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
        BTreeMap::from([(
            String::from("AGENT_OS_PYODIDE_PACKAGE_BASE_URL"),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::from([(
            String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
            serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
        )]),
        PermissionsPolicy {
            fs: PermissionsPolicy::allow_all().fs,
            network: Some(PatternPermissionScope::Mode(PermissionMode::Deny)),
            child_process: PermissionsPolicy::allow_all().child_process,
            process: PermissionsPolicy::allow_all().process,
            env: PermissionsPolicy::allow_all().env,
            tool: PermissionsPolicy::allow_all().tool,
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
        BTreeMap::from([(
            String::from("AGENT_OS_PYODIDE_PACKAGE_BASE_URL"),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::from([
            (
                String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
                serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
            ),
            (
                String::from("network.dns.override.example.test"),
                String::from("127.0.0.1"),
            ),
        ]),
        PermissionsPolicy::allow_all(),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::from([
            (
                String::from("env.AGENT_OS_LOOPBACK_EXEMPT_PORTS"),
                serde_json::to_string(&vec![port.to_string()]).expect("serialize exempt ports"),
            ),
            (
                String::from("network.dns.override.example.test"),
                String::from("127.0.0.1"),
            ),
        ]),
        PermissionsPolicy::allow_all(),
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::from([(
            String::from("network.dns.override.example.test"),
            String::from("127.0.0.1"),
        )]),
        PermissionsPolicy {
            fs: PermissionsPolicy::allow_all().fs,
            network: Some(PatternPermissionScope::Mode(PermissionMode::Deny)),
            child_process: PermissionsPolicy::allow_all().child_process,
            process: PermissionsPolicy::allow_all().process,
            env: PermissionsPolicy::allow_all().env,
            tool: PermissionsPolicy::allow_all().tool,
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
        Value::String(String::from("PermissionError"))
    );
}

fn python_runtime_runs_node_subprocesses_through_sidecar_bridge() {
    assert_node_available();

    let mut sidecar = new_sidecar("python-subprocess-bridge");
    let cwd = temp_dir("python-subprocess-bridge-cwd");
    write_fixture(&cwd.join("child.mjs"), "console.log('child-ready')\n");
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    let connection_id = authenticate(&mut sidecar, "conn-python");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = create_vm_with_metadata_and_permissions(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::Python,
        &cwd,
        BTreeMap::new(),
        PermissionsPolicy {
            fs: PermissionsPolicy::allow_all().fs,
            network: PermissionsPolicy::allow_all().network,
            child_process: Some(PatternPermissionScope::Rules(
                agent_os_sidecar::protocol::PatternPermissionRuleSet {
                    default: Some(PermissionMode::Allow),
                    rules: vec![agent_os_sidecar::protocol::PatternPermissionRule {
                        mode: PermissionMode::Deny,
                        operations: vec![String::from("*")],
                        patterns: vec![String::from("node")],
                    }],
                },
            )),
            process: PermissionsPolicy::allow_all().process,
            env: PermissionsPolicy::allow_all().env,
            tool: PermissionsPolicy::allow_all().tool,
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

#[test]
fn python_suite() {
    // Multiple libtest cases in this V8/Pyodide-backed integration binary
    // still trip teardown/init crashes, so keep the coverage in one suite.
    python_runtime_executes_code_end_to_end();
    python_runtime_executes_workspace_py_file_by_path();
    python_runtime_reports_syntax_errors_over_stderr();
    python_runtime_blocks_pyodide_js_escape_hatches();
    concurrent_python_processes_stay_isolated_across_vms();
    python_runtime_mounts_workspace_over_the_kernel_vfs();
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
}
