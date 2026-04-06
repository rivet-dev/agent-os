mod support;

use agent_os_sidecar::protocol::{
    GuestRuntimeKind, OwnershipScope, RequestPayload, ResponsePayload, WriteStdinRequest,
};
use agent_os_sidecar::{NativeSidecar, NativeSidecarConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
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
