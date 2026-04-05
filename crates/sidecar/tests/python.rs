mod support;

use agent_os_sidecar::protocol::{
    BootstrapRootFilesystemRequest, CloseStdinRequest, ConfigureVmRequest, CreateVmRequest,
    EventPayload, ExecuteRequest, GuestFilesystemCallRequest, GuestFilesystemOperation,
    GuestRuntimeKind, KillProcessRequest, MountDescriptor, MountPluginDescriptor, OwnershipScope,
    RequestPayload, ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry,
    RootFilesystemEntryEncoding, RootFilesystemEntryKind, RootFilesystemMode, StreamChannel,
    WriteStdinRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
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

fn execute_inline_python(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: u64,
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
    request_id: u64,
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
    request_id: u64,
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &str,
    env: BTreeMap<String, String>,
) {
    let result = sidecar
        .dispatch(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: process_id.to_owned(),
                runtime: GuestRuntimeKind::Python,
                entrypoint: entrypoint.to_owned(),
                args: Vec::new(),
                env,
                cwd: None,
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &Path,
    args: Vec<String>,
    env: BTreeMap<String, String>,
) {
    let result = sidecar
        .dispatch(support::request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: process_id.to_owned(),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: entrypoint.to_string_lossy().into_owned(),
                args,
                env,
                cwd: None,
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    root_filesystem: RootFilesystemDescriptor,
) -> String {
    let result = sidecar
        .dispatch(support::request(
            request_id,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem,
                permissions: Vec::new(),
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    entries: Vec<RootFilesystemEntry>,
) {
    let result = sidecar
        .dispatch(support::request(
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    payload: GuestFilesystemCallRequest,
) -> agent_os_sidecar::protocol::GuestFilesystemResultResponse {
    let result = sidecar
        .dispatch(support::request(
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
    request_id: u64,
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
    request_id: u64,
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    chunk: &str,
) {
    let result = sidecar
        .dispatch(support::request(
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch(support::request(
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
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) {
    let result = sidecar
        .dispatch(support::request(
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
            .poll_event(&ownership, Duration::from_millis(100))
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

#[test]
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

#[test]
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

#[test]
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

#[test]
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

    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse python security JSON");
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

#[test]
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
            .poll_event(&ownership, Duration::from_millis(100))
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

#[test]
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

#[test]
fn workspace_files_are_shared_between_javascript_and_python_runtimes() {
    assert_node_available();

    let mut sidecar = new_sidecar("cross-runtime-workspace");
    let cwd = temp_dir("cross-runtime-workspace-cwd");
    let js_entry = cwd.join("cross-runtime.mjs");
    let workspace_host_dir = temp_dir("cross-runtime-workspace-host");
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
import * as fs from 'agent-os:builtin/fs-promises';

const mode = process.argv[2];

if (mode === 'write') {
  await fs.writeFile('/workspace/from-js.txt', 'from js', 'utf8');
  console.log(JSON.stringify({
    entries: (await fs.readdir('/workspace')).sort(),
  }));
} else if (mode === 'read') {
  console.log(JSON.stringify({
    fromPython: await fs.readFile('/workspace/from-python.txt', 'utf8'),
    entries: (await fs.readdir('/workspace')).sort(),
  }));
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
        .dispatch(support::request(
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
                permissions: Vec::new(),
                instructions: Vec::new(),
                projected_modules: Vec::new(),
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
    let js_write: Value =
        serde_json::from_str(js_write_stdout.trim()).expect("parse JavaScript write JSON");
    assert_eq!(js_write["entries"], serde_json::json!(["from-js.txt"]));

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
        serde_json::json!(["from-js.txt", "from-python.txt"])
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
    let js_read: Value =
        serde_json::from_str(js_read_stdout.trim()).expect("parse JavaScript read JSON");
    assert_eq!(js_read["fromPython"], "from python");
    assert_eq!(
        js_read["entries"],
        serde_json::json!(["from-js.txt", "from-python.txt"])
    );
}

#[test]
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

#[test]
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
            .poll_event(
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

#[test]
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
            .poll_event(
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
            .poll_event(
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

#[test]
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

#[test]
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
        stderr.is_empty() || stderr.contains("terminated") || stderr.contains("SIGTERM"),
        "unexpected python kill stderr: {stderr}"
    );
}

#[test]
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

#[test]
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
