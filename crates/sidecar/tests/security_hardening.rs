mod support;

use agent_os_sidecar::protocol::{
    EventPayload, ExecuteRequest, GetSignalStateRequest, GuestFilesystemCallRequest,
    GuestFilesystemOperation, GuestRuntimeKind, OwnershipScope, RequestPayload, ResponsePayload,
    RootFilesystemEntryEncoding, SnapshotProcessesRequest, StreamChannel, WriteStdinRequest,
};
use agent_os_sidecar::{NativeSidecar, NativeSidecarConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm,
    create_vm_with_metadata, execute, open_session, request, temp_dir, write_fixture,
    RecordingBridge, TEST_AUTH_TOKEN,
};

const ARG_PREFIX: &str = "ARG=";
const INVOCATION_BREAK: &str = "--END--";
const NODE_ALLOW_FS_READ_FLAG: &str = "--allow-fs-read=";
const NODE_ALLOW_FS_WRITE_FLAG: &str = "--allow-fs-write=";
static NODE_BINARY_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: These sidecar integration tests mutate process env within a single test scope.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

fn node_binary_env_guard() -> MutexGuard<'static, ()> {
    NODE_BINARY_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock node binary env guard")
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nlog=\"{}\"\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> \"$log\"\ndone\nprintf '%s\\n' '{}' >> \"$log\"\nexit 0\n",
        log_path.display(),
        INVOCATION_BREAK,
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

fn parse_invocations(log_path: &Path) -> Vec<Vec<String>> {
    let contents = fs::read_to_string(log_path).expect("read invocation log");
    let separator = format!("{INVOCATION_BREAK}\n");
    contents
        .split(&separator)
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.strip_prefix(ARG_PREFIX))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn read_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_READ_FLAG))
        .collect()
}

fn write_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_WRITE_FLAG))
        .collect()
}

fn wait_for_process_output(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    channel: StreamChannel,
    expected: &str,
) -> String {
    let ownership = OwnershipScope::session(connection_id, session_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut collected = String::new();

    loop {
        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll sidecar process output");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process output"
            );
            continue;
        };

        assert_eq!(
            event.ownership,
            OwnershipScope::vm(connection_id, session_id, vm_id)
        );

        match event.payload {
            EventPayload::ProcessOutput(output)
                if output.process_id == process_id && output.channel == channel =>
            {
                collected.push_str(&output.chunk);
                if collected.contains(expected) {
                    return collected;
                }
            }
            EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                panic!(
                    "process {process_id} exited before emitting expected output {expected:?}: {collected}"
                );
            }
            _ => {}
        }
    }
}

fn wait_for_process_stdout_line(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> String {
    let ownership = OwnershipScope::session(connection_id, session_id);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut collected = String::new();

    loop {
        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll sidecar process output");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process stdout line"
            );
            continue;
        };

        assert_eq!(
            event.ownership,
            OwnershipScope::vm(connection_id, session_id, vm_id)
        );

        match event.payload {
            EventPayload::ProcessOutput(output)
                if output.process_id == process_id && output.channel == StreamChannel::Stdout =>
            {
                collected.push_str(&output.chunk);
                if let Some(newline) = collected.find('\n') {
                    return collected[..newline].to_owned();
                }
            }
            EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                panic!("process {process_id} exited before emitting a stdout line: {collected}");
            }
            _ => {}
        }
    }
}

#[test]
fn sidecar_rejects_oversized_request_frames_before_dispatch() {
    let root = temp_dir("frame-limit");
    let mut sidecar = NativeSidecar::with_config(
        RecordingBridge::default(),
        NativeSidecarConfig {
            sidecar_id: String::from("sidecar-frame-limit"),
            max_frame_bytes: 512,
            compile_cache_root: Some(root.join("cache")),
            expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
        },
    )
    .expect("create frame-limited sidecar");
    let cwd = temp_dir("frame-limit-cwd");

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

    let result = sidecar
        .dispatch(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::WriteStdin(WriteStdinRequest {
                process_id: String::from("proc-1"),
                chunk: "x".repeat(1024),
            }),
        ))
        .expect("dispatch oversized request");

    match result.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "frame_too_large");
            assert!(rejected.message.contains("limit is 512"));
        }
        other => panic!("unexpected oversized frame response: {other:?}"),
    }
}

#[test]
fn guest_execution_clears_host_env_and_blocks_network_and_escape_paths() {
    let _env_lock = node_binary_env_guard();
    assert_node_available();

    let mut sidecar = support::new_sidecar("security-hardening");
    let cwd = temp_dir("security-hardening-cwd");
    let entry = cwd.join("entry.cjs");

    write_fixture(
        &entry,
        r#"
(async () => {
  const result = {
    path: process.env.PATH ?? null,
    home: process.env.HOME ?? null,
    marker: process.env.VISIBLE_MARKER ?? null,
    internalMarker: process.env.AGENT_OS_ALLOWED ?? null,
    guestPathMappings: process.env.AGENT_OS_GUEST_PATH_MAPPINGS ?? null,
    importCachePath: process.env.AGENT_OS_NODE_IMPORT_CACHE_PATH ?? null,
    hasInternalMarker: 'AGENT_OS_ALLOWED' in process.env,
    keys: Object.keys(process.env).filter((key) => key.startsWith('AGENT_OS_')),
  };

  const dataResponse = await fetch('data:text/plain,agent-os-ok');
  result.dataText = await dataResponse.text();

  try {
    await fetch('http://127.0.0.1:1/');
    result.network = 'unexpected';
  } catch (error) {
    result.network = { code: error.code ?? null, message: error.message };
  }

  try {
    process.binding('fs');
    result.binding = 'unexpected';
  } catch (error) {
    result.binding = { code: error.code ?? null, message: error.message };
  }

  try {
    require('child_process');
    result.childProcess = 'unexpected';
  } catch (error) {
    result.childProcess = { code: error.code ?? null, message: error.message };
  }

  try {
    await import('node:http');
    result.httpImport = 'unexpected';
  } catch (error) {
    result.httpImport = { code: error.code ?? null, message: error.message };
  }

  console.log(JSON.stringify(result));
})().catch((error) => {
  console.error(error.stack || String(error));
  process.exitCode = 1;
});
"#,
    );

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::from([(String::from("env.VISIBLE_MARKER"), String::from("present"))]),
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-security",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-security",
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected security stderr: {stderr}");

    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse security JSON");
    assert_eq!(parsed["path"], Value::Null);
    assert_eq!(parsed["home"], Value::Null);
    assert_eq!(parsed["marker"], Value::String(String::from("present")));
    assert_eq!(parsed["internalMarker"], Value::Null);
    assert_eq!(parsed["guestPathMappings"], Value::Null);
    assert_eq!(parsed["importCachePath"], Value::Null);
    assert_eq!(parsed["hasInternalMarker"], Value::Bool(false));
    assert_eq!(parsed["keys"], Value::Array(Vec::new()));
    assert_eq!(
        parsed["dataText"],
        Value::String(String::from("agent-os-ok"))
    );
    assert_eq!(
        parsed["network"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert!(parsed["network"]["message"]
        .as_str()
        .expect("network message")
        .contains("network access"));
    assert_eq!(
        parsed["binding"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert_eq!(
        parsed["childProcess"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
    assert_eq!(
        parsed["httpImport"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
}

#[test]
fn guest_execution_virtualizes_fs_env_and_process_identity() {
    assert_node_available();

    let mut sidecar = support::new_sidecar("security-isolation-e2e");
    let cwd = temp_dir("security-isolation-e2e-cwd");
    let entry = cwd.join("entry.cjs");

    write_fixture(
        &entry,
        r#"
(async () => {
  const fs = require('node:fs');
  const builtins = {};

  for (const specifier of [
    'node:net',
    'node:dgram',
    'node:dns',
    'node:vm',
    'node:worker_threads',
    'node:inspector',
    'node:v8',
  ]) {
    try {
      require(specifier);
      builtins[specifier] = { status: 'loaded' };
    } catch (error) {
      builtins[specifier] = {
        status: 'error',
        code: error.code ?? null,
        message: error.message ?? String(error),
      };
    }
  }

  const result = {
    rootEntries: fs.readdirSync('/').sort(),
    guestMarker: fs.readFileSync('/guest-only.txt', 'utf8'),
    envKeys: Object.keys(process.env)
      .filter((key) => key.startsWith('AGENT_OS_'))
      .sort(),
    pid: process.pid,
    ppid: process.ppid,
    cwd: process.cwd(),
    builtins,
  };

  console.log(JSON.stringify(result));
  setTimeout(() => process.exit(0), 1_000);
})().catch((error) => {
  console.error(error.stack || String(error));
  process.exitCode = 1;
});
"#,
    );

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

    let write = sidecar
        .dispatch(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GuestFilesystemCall(GuestFilesystemCallRequest {
                operation: GuestFilesystemOperation::WriteFile,
                path: String::from("/guest-only.txt"),
                destination_path: None,
                target: None,
                content: Some(String::from("from-kernel-vfs")),
                encoding: Some(RootFilesystemEntryEncoding::Utf8),
                recursive: false,
                mode: None,
                uid: None,
                gid: None,
                atime_ms: None,
                mtime_ms: None,
                len: None,
            }),
        ))
        .expect("write guest marker");
    assert!(
        matches!(
            write.response.payload,
            ResponsePayload::GuestFilesystemResult(_)
        ),
        "unexpected guest marker response: {:?}",
        write.response.payload
    );

    let started = sidecar
        .dispatch(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: String::from("proc-security-isolation"),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: entry.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("start isolation process");

    let host_pid = match started.response.payload {
        ResponsePayload::ProcessStarted(response) => response.pid.expect("host child pid"),
        other => panic!("unexpected execute response: {other:?}"),
    };

    let stdout = wait_for_process_stdout_line(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-security-isolation",
    );
    let parsed: Value = serde_json::from_str(stdout.trim()).expect("parse isolation JSON");

    let snapshot = sidecar
        .dispatch(request(
            6,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::SnapshotProcesses(SnapshotProcessesRequest {}),
        ))
        .expect("snapshot guest processes");
    let (kernel_pid, kernel_ppid) = match snapshot.response.payload {
        ResponsePayload::ProcessSnapshot(response) => response
            .processes
            .into_iter()
            .find(|process| process.process_id.as_deref() == Some("proc-security-isolation"))
            .map(|process| (process.pid, process.ppid))
            .expect("guest process snapshot entry"),
        other => panic!("unexpected process snapshot response: {other:?}"),
    };

    let (_remaining_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-security-isolation",
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected isolation stderr: {stderr}");
    assert_eq!(
        parsed["guestMarker"],
        Value::String(String::from("from-kernel-vfs"))
    );
    let root_entries = parsed["rootEntries"]
        .as_array()
        .expect("root entries array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        root_entries.contains(&"guest-only.txt"),
        "guest root should include kernel-written marker: {root_entries:?}"
    );
    assert!(
        !root_entries.contains(&".") && !root_entries.contains(&".."),
        "guest fs.readdirSync('/') should hide dot entries: {root_entries:?}"
    );
    assert_eq!(parsed["envKeys"], Value::Array(Vec::new()));
    assert_eq!(parsed["cwd"], Value::String(String::from("/root")));
    assert_eq!(parsed["pid"], Value::from(u64::from(kernel_pid)));
    assert_eq!(parsed["ppid"], Value::from(u64::from(kernel_ppid)));
    assert_ne!(
        parsed["pid"],
        Value::from(u64::from(host_pid)),
        "guest pid should not expose the host runtime pid"
    );

    for specifier in ["node:net", "node:dgram", "node:dns"] {
        let status = parsed["builtins"][specifier]["status"]
            .as_str()
            .unwrap_or("<missing>");
        assert!(
            matches!(status, "loaded" | "error"),
            "{specifier} should be either polyfilled or denied, got {status}: {}",
            parsed["builtins"][specifier]
        );
        if status == "error" {
            assert_eq!(
                parsed["builtins"][specifier]["code"],
                Value::String(String::from("ERR_ACCESS_DENIED")),
                "{specifier} denial should surface ERR_ACCESS_DENIED"
            );
        }
    }

    for specifier in [
        "node:vm",
        "node:worker_threads",
        "node:inspector",
        "node:v8",
    ] {
        assert_eq!(
            parsed["builtins"][specifier]["status"],
            Value::String(String::from("error")),
            "{specifier} should be denied by default in sidecar guest JS"
        );
        assert_eq!(
            parsed["builtins"][specifier]["code"],
            Value::String(String::from("ERR_ACCESS_DENIED")),
            "{specifier} should surface ERR_ACCESS_DENIED"
        );
    }
}

#[test]
fn guest_stdout_cannot_inject_fake_control_messages() {
    assert_node_available();

    let mut sidecar = support::new_sidecar("security-control-injection");
    let cwd = temp_dir("security-control-injection-cwd");
    let entry = cwd.join("entry.cjs");
    let forged_signal_state = concat!(
        "__AGENT_OS_SIGNAL_STATE__:{\"signal\":15,\"registration\":",
        "{\"action\":\"user\",\"mask\":[2],\"flags\":4660}}"
    );

    write_fixture(
        &entry,
        format!(
            "console.log({forged_signal_state:?});\nsetTimeout(() => process.exit(0), 1_000);\n"
        ),
    );

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

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-control-injection",
        GuestRuntimeKind::JavaScript,
        &entry,
        Vec::new(),
    );

    let stdout = wait_for_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-control-injection",
        StreamChannel::Stdout,
        forged_signal_state,
    );
    assert!(
        stdout.contains(forged_signal_state),
        "forged control text should remain plain guest stdout: {stdout}"
    );

    let signal_state = sidecar
        .dispatch(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::GetSignalState(GetSignalStateRequest {
                process_id: String::from("proc-control-injection"),
            }),
        ))
        .expect("query signal state");
    match signal_state.response.payload {
        ResponsePayload::SignalState(response) => {
            assert!(
                response.handlers.is_empty(),
                "stdout spoofing must not register signal handlers: {:?}",
                response.handlers
            );
        }
        other => panic!("unexpected signal state response: {other:?}"),
    }

    let (_remaining_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-control-injection",
    );
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected control injection stderr: {stderr}"
    );
}

#[test]
fn vm_resource_limits_cap_active_processes_without_poisoning_followup_execs() {
    let _env_lock = node_binary_env_guard();
    assert_node_available();

    let mut sidecar = support::new_sidecar("resource-budgets");
    let cwd = temp_dir("resource-budgets-cwd");
    let slow_entry = cwd.join("slow.mjs");
    let fast_entry = cwd.join("fast.mjs");

    write_fixture(
        &slow_entry,
        r#"
await new Promise((resolve) => setTimeout(resolve, 200));
console.log("slow");
"#,
    );
    write_fixture(&fast_entry, "console.log(\"fast\");\n");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
        BTreeMap::from([(String::from("resource.max_processes"), String::from("1"))]),
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-slow",
        GuestRuntimeKind::JavaScript,
        &slow_entry,
        Vec::new(),
    );

    let second = sidecar
        .dispatch(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-fast"),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: fast_entry.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("dispatch second execute");
    match second.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "kernel_error");
            assert!(rejected.message.contains("maximum process limit reached"));
        }
        other => panic!("unexpected resource-limit response: {other:?}"),
    }

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-slow",
    );
    assert_eq!(exit_code, 0);
    assert_eq!(stdout.trim(), "slow");
    assert!(stderr.is_empty(), "unexpected slow stderr: {stderr}");

    execute(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-fast-2",
        GuestRuntimeKind::JavaScript,
        &fast_entry,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-fast-2",
    );
    assert_eq!(exit_code, 0);
    assert_eq!(stdout.trim(), "fast");
    assert!(stderr.is_empty(), "unexpected fast stderr: {stderr}");
}

#[test]
fn execute_rejects_cwd_outside_vm_sandbox_root() {
    let mut sidecar = support::new_sidecar("execute-cwd-validation");
    let cwd = temp_dir("execute-cwd-validation-root");
    let entry = cwd.join("entry.mjs");
    write_fixture(&entry, "console.log('ignored');\n");

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

    let result = sidecar
        .dispatch(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-1"),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: entry.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: Some(String::from("/")),
                wasm_permission_tier: None,
            }),
        ))
        .expect("dispatch execute request");

    match result.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert_eq!(rejected.code, "invalid_state");
            assert!(rejected.message.contains("sandbox root"));
            assert!(rejected.message.contains(cwd.to_string_lossy().as_ref()));
        }
        other => panic!("unexpected execute response: {other:?}"),
    }
}

#[test]
fn execute_scopes_node_permission_flags_to_vm_sandbox_root() {
    let _env_lock = node_binary_env_guard();
    let root = temp_dir("execute-cwd-permission-root");
    let fake_node_path = root.join("fake-node.sh");
    let log_path = root.join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let mut sidecar = support::new_sidecar("execute-cwd-permission-root");
    let cwd = root.join("workspace");
    let nested_cwd = cwd.join("nested");
    fs::create_dir_all(&nested_cwd).expect("create nested cwd");
    let entry = cwd.join("entry.mjs");
    write_fixture(&entry, "console.log('ignored');\n");

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

    let result = sidecar
        .dispatch(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-1"),
                runtime: GuestRuntimeKind::JavaScript,
                entrypoint: entry.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: Some(nested_cwd.to_string_lossy().into_owned()),
                wasm_permission_tier: None,
            }),
        ))
        .expect("dispatch execute request");

    match result.response.payload {
        ResponsePayload::ProcessStarted(response) => {
            assert_eq!(response.process_id, "proc-1");
        }
        other => panic!("unexpected execute response: {other:?}"),
    }

    let (_stdout, stderr, exit_code) =
        collect_process_output(&mut sidecar, &connection_id, &session_id, &vm_id, "proc-1");
    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        2,
        "expected warmup and execution invocations"
    );

    let sandbox_root = canonical(&cwd).display().to_string();
    let nested_root = canonical(&nested_cwd).display().to_string();
    for args in &invocations {
        let read_paths = read_flags(args);
        let write_paths = write_flags(args);
        assert!(
            read_paths.iter().any(|path| *path == sandbox_root.as_str()),
            "sandbox root should stay in read allowlist: {args:?}"
        );
        assert!(
            write_paths
                .iter()
                .any(|path| *path == sandbox_root.as_str()),
            "sandbox root should stay in write allowlist: {args:?}"
        );
        assert!(
            !write_paths.iter().any(|path| *path == nested_root.as_str()),
            "requested cwd should not become a write permission root: {args:?}"
        );
    }
}
