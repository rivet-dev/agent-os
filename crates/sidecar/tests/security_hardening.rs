mod support;

use agent_os_sidecar::protocol::{
    ConfigureVmRequest, CreateVmRequest, GuestRuntimeKind, OwnershipScope, PermissionsPolicy,
    RequestPayload, ResponsePayload, WriteStdinRequest,
};
use agent_os_sidecar::{NativeSidecar, NativeSidecarConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;
use support::{
    acquire_sidecar_runtime_test_lock, assert_node_available, authenticate, collect_process_output,
    create_vm, create_vm_with_metadata, execute, open_session, request, temp_dir, write_fixture,
    RecordingBridge, TEST_AUTH_TOKEN,
};

const ARG_PREFIX: &str = "ARG=";
const INVOCATION_BREAK: &str = "--END--";
const DEFAULT_GUEST_PATH_ENV: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const DEFAULT_GUEST_HOME: &str = "/home/user";
struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set_value(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: These sidecar integration tests mutate process env within a single test scope.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn set_path(key: &'static str, value: &Path) -> Self {
        Self::set_value(key, value.as_os_str())
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

fn sidecar_rejects_oversized_request_frames_before_dispatch() {
    acquire_sidecar_runtime_test_lock();
    let root = temp_dir("frame-limit");
    let mut sidecar = NativeSidecar::with_config(
        RecordingBridge::default(),
        NativeSidecarConfig {
            sidecar_id: String::from("sidecar-frame-limit"),
            max_frame_bytes: 512,
            compile_cache_root: Some(root.join("cache")),
            expected_auth_token: Some(String::from(TEST_AUTH_TOKEN)),
            acp_termination_grace: Duration::from_secs(3),
        },
    )
    .expect("create frame-limited sidecar");
    let cwd = temp_dir("frame-limit-cwd");

    let connection_id = authenticate(&mut sidecar, "conn-1");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let vm_id = match sidecar
        .dispatch_blocking(request(
            3,
            OwnershipScope::session(&connection_id, &session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem: Default::default(),
                permissions: None,
            }),
        ))
        .expect("create frame-limit vm")
        .response
        .payload
    {
        ResponsePayload::VmCreated(response) => response.vm_id,
        other => panic!("unexpected vm create response: {other:?}"),
    };

    let result = sidecar
        .dispatch_blocking(request(
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

fn guest_execution_clears_host_env_and_blocks_escape_paths() {
    assert_node_available();

    let _host_path = EnvVarGuard::set_value("PATH", "/host/sbin:/host/bin");
    let _host_home = EnvVarGuard::set_value("HOME", "/host/home");
    let _host_internal = EnvVarGuard::set_value("AGENT_OS_ALLOWED", "host-internal");
    let mut sidecar = support::new_sidecar("security-hardening");
    let cwd = temp_dir("security-hardening-cwd");
    let entry = cwd.join("entry.cjs");

    write_fixture(
        &entry,
        r#"
const result = {
  path: process.env.PATH ?? null,
  home: process.env.HOME ?? null,
  pwd: process.env.PWD ?? null,
  marker: process.env.VISIBLE_MARKER ?? null,
  internalMarker: process.env.AGENT_OS_ALLOWED ?? null,
  guestPathMappings: process.env.AGENT_OS_GUEST_PATH_MAPPINGS ?? null,
  importCachePath: process.env.AGENT_OS_NODE_IMPORT_CACHE_PATH ?? null,
  hasInternalMarker: 'AGENT_OS_ALLOWED' in process.env,
  keys: Object.keys(process.env).filter((key) => key.startsWith('AGENT_OS_')),
};

try {
  process.binding('fs');
  result.binding = 'unexpected';
} catch (error) {
  result.binding = { code: error.code ?? null, message: error.message };
}

console.log(JSON.stringify(result));
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
    let (_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-security",
    );
    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected security stderr: {stderr}");

    let parsed: Value = serde_json::from_str(_stdout.trim()).expect("parse security JSON");
    assert_eq!(
        parsed["path"],
        Value::String(String::from(DEFAULT_GUEST_PATH_ENV))
    );
    assert_eq!(
        parsed["home"],
        Value::String(String::from(DEFAULT_GUEST_HOME))
    );
    assert_eq!(parsed["pwd"], Value::String(String::from("/")));
    assert_eq!(parsed["marker"], Value::String(String::from("present")));
    assert_eq!(parsed["internalMarker"], Value::Null);
    assert_eq!(parsed["guestPathMappings"], Value::Null);
    assert_eq!(parsed["importCachePath"], Value::Null);
    assert_eq!(parsed["hasInternalMarker"], Value::Bool(false));
    assert_eq!(parsed["keys"], Value::Array(Vec::new()));
    assert_ne!(
        parsed["path"],
        Value::String(String::from("/host/sbin:/host/bin"))
    );
    assert_ne!(parsed["home"], Value::String(String::from("/host/home")));
    assert_eq!(
        parsed["binding"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
}

fn vm_resource_limits_cap_active_processes_without_poisoning_followup_execs() {
    assert_node_available();

    let mut sidecar = support::new_sidecar("resource-budgets");
    let cwd = temp_dir("resource-budgets-cwd");
    let slow_entry = cwd.join("slow.cjs");
    let fast_entry = cwd.join("fast.cjs");

    write_fixture(&slow_entry, "setTimeout(() => {}, 200);\n");
    write_fixture(&fast_entry, "void 0;\n");

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
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-fast"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(fast_entry.to_string_lossy().into_owned()),
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

    let (_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-slow",
    );
    assert_eq!(exit_code, 0);
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
    let (_stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-fast-2",
    );
    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected fast stderr: {stderr}");
}

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
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-1"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(entry.to_string_lossy().into_owned()),
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

fn execute_rejects_host_only_absolute_command_path() {
    let mut sidecar = support::new_sidecar("execute-host-only-command");
    let cwd = temp_dir("execute-host-only-command-cwd");
    let host_only_root = temp_dir("execute-host-only-command-host");
    let host_only_command = host_only_root.join("host-only-command.sh");
    write_fixture(
        &host_only_command,
        "#!/bin/sh\nprintf 'host-only command should stay hidden\\n'\n",
    );
    let mut permissions = fs::metadata(&host_only_command)
        .expect("host-only command metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&host_only_command, permissions).expect("chmod host-only command");

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
    sidecar
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ConfigureVm(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: Some(PermissionsPolicy::allow_all()),
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: Default::default(),
                allowed_node_builtins: Vec::new(),
                loopback_exempt_ports: Vec::new(),
            }),
        ))
        .expect("configure host-only command permissions");

    let result = sidecar
        .dispatch_blocking(request(
            5,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-host-only"),
                command: Some(host_only_command.to_string_lossy().into_owned()),
                runtime: None,
                entrypoint: None,
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
                wasm_permission_tier: None,
            }),
        ))
        .expect("dispatch host-only command execute");

    match result.response.payload {
        ResponsePayload::Rejected(rejected) => {
            assert!(
                rejected.code == "kernel_error"
                    || rejected.code == "execution_error"
                    || rejected.code == "invalid_state",
                "unexpected rejection code: {rejected:?}"
            );
            if rejected.code == "invalid_state" {
                assert!(
                    rejected
                        .message
                        .contains("command not found on native sidecar path"),
                    "unexpected invalid_state rejection: {rejected:?}"
                );
            }
            assert!(
                !rejected
                    .message
                    .contains("host-only command should stay hidden"),
                "host-only command output should not leak through the rejection: {rejected:?}"
            );
        }
        other => panic!("unexpected execute response: {other:?}"),
    }
}

fn execute_ignores_host_node_binary_override_for_javascript_runtime() {
    let root = temp_dir("execute-cwd-permission-root");
    let fake_node_path = root.join("fake-node.sh");
    let log_path = root.join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set_path("AGENT_OS_NODE_BINARY", &fake_node_path);

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
        .dispatch_blocking(request(
            4,
            OwnershipScope::vm(&connection_id, &session_id, &vm_id),
            RequestPayload::Execute(agent_os_sidecar::protocol::ExecuteRequest {
                process_id: String::from("proc-1"),
                command: None,
                runtime: Some(GuestRuntimeKind::JavaScript),
                entrypoint: Some(entry.to_string_lossy().into_owned()),
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

    assert!(
        !log_path.exists(),
        "javascript guest execution should stay inside the V8 runtime instead of invoking host node: {:?}",
        parse_invocations(&log_path)
    );
}

#[test]
fn security_hardening_suite() {
    // Multiple libtest cases in this V8-backed integration binary still trip
    // teardown/init crashes, so keep the coverage in one top-level suite.
    execute_ignores_host_node_binary_override_for_javascript_runtime();
    execute_rejects_cwd_outside_vm_sandbox_root();
    execute_rejects_host_only_absolute_command_path();
    guest_execution_clears_host_env_and_blocks_escape_paths();
    sidecar_rejects_oversized_request_frames_before_dispatch();
    vm_resource_limits_cap_active_processes_without_poisoning_followup_execs();
}
