mod support;

use agentos_vm::wire::{
    ExecuteRequest, GuestRuntimeKind, RequestId, RequestPayload, ResponsePayload,
    RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind, RootFilesystemMode,
};
use base64::Engine as _;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::process::Command;
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_request_with_selected_wasm_backend, create_vm_wire, dispose_vm_and_close_session,
    execute_wire, new_sidecar, open_session_wire, temp_dir, wire_permissions_allow_all,
    wire_request, wire_session,
};

const DEFAULT_GUEST_PATH_ENV: &str =
    "/usr/local/sbin:/usr/local/bin:/opt/agentos/bin:/usr/sbin:/usr/bin:/sbin:/bin";
const GUEST_IDENTITY_CASES: &[&str] = &[
    "javascript",
    "python",
    "wasm_identity",
    "wasm_pty",
    "wasm_env",
    "wasm_preopen",
];

fn create_vm_with_root_filesystem(
    sidecar: &mut agentos_vm::VmManager<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &std::path::Path,
    root_filesystem: RootFilesystemDescriptor,
) -> String {
    create_vm_with_root_filesystem_and_metadata(
        sidecar,
        request_id,
        connection_id,
        session_id,
        runtime,
        cwd,
        root_filesystem,
        HashMap::new(),
    )
}

#[allow(clippy::too_many_arguments)]
fn create_vm_with_root_filesystem_and_metadata(
    sidecar: &mut agentos_vm::VmManager<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &std::path::Path,
    root_filesystem: RootFilesystemDescriptor,
    mut metadata: HashMap<String, String>,
) -> String {
    metadata
        .entry(String::from("cwd"))
        .or_insert_with(|| cwd.to_string_lossy().into_owned());
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_session(connection_id, session_id),
            RequestPayload::CreateVmRequest(create_vm_request_with_selected_wasm_backend(
                runtime,
                metadata,
                root_filesystem,
                Some(wire_permissions_allow_all()),
            )),
        ))
        .expect("create sidecar VM");

    match result.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected vm create response: {other:?}"),
    }
}

fn parse_json_stdout(stdout: &str) -> Value {
    serde_json::from_str(stdout.trim()).expect("parse JSON stdout")
}

fn parse_env_stdout(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

fn executable_wasm_root_entry(path: &str, bytes: &[u8]) -> RootFilesystemEntry {
    RootFilesystemEntry {
        path: path.to_owned(),
        kind: RootFilesystemEntryKind::File,
        mode: None,
        uid: None,
        gid: None,
        content: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        encoding: Some(RootFilesystemEntryEncoding::Base64),
        target: None,
        executable: true,
    }
}

fn javascript_guest_identity_uses_kernel_owned_defaults() {
    let mut sidecar = new_sidecar("guest-identity-js");
    let cwd = temp_dir("guest-identity-js-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-identity-js");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        &cwd,
    );

    let entrypoint = cwd.join("identity.mjs");
    fs::write(
        &entrypoint,
        r#"
import os from "node:os";

console.log(JSON.stringify({
  envUser: process.env.USER ?? null,
  envHome: process.env.HOME ?? null,
  envPwd: process.env.PWD ?? null,
  envShell: process.env.SHELL ?? null,
  envPath: process.env.PATH ?? null,
  internalKeys: Object.keys(process.env).filter((key) =>
    key.startsWith("AGENTOS_") || key.startsWith("NODE_SYNC_RPC_")
  ),
  uid: process.getuid(),
  gid: process.getgid(),
  euid: process.geteuid(),
  egid: process.getegid(),
  groups: process.getgroups(),
  homedir: os.homedir(),
  userInfo: os.userInfo(),
  cwd: process.cwd(),
}));
"#,
    )
    .expect("write JavaScript identity fixture");

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-identity",
        GuestRuntimeKind::JavaScript,
        &entrypoint,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-identity",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from JavaScript identity execution: {stderr}"
    );

    let parsed = parse_json_stdout(&stdout);
    assert_eq!(parsed["envUser"], "agentos");
    assert_eq!(parsed["envHome"], "/home/agentos");
    assert_eq!(parsed["envPwd"], "/");
    assert_eq!(parsed["envShell"], "/bin/sh");
    assert_eq!(parsed["envPath"], DEFAULT_GUEST_PATH_ENV);
    assert_eq!(parsed["internalKeys"], Value::Array(Vec::new()));
    assert_eq!(parsed["uid"], 1000);
    assert_eq!(parsed["gid"], 1000);
    assert_eq!(parsed["euid"], 1000);
    assert_eq!(parsed["egid"], 1000);
    assert_eq!(parsed["groups"], Value::Array(vec![Value::from(1000)]));
    assert_eq!(parsed["homedir"], "/home/agentos");
    assert_eq!(parsed["cwd"], "/");
    assert_eq!(parsed["userInfo"]["username"], "agentos");
    assert_eq!(parsed["userInfo"]["uid"], 1000);
    assert_eq!(parsed["userInfo"]["gid"], 1000);
    assert_eq!(parsed["userInfo"]["shell"], "/bin/sh");
    assert_eq!(parsed["userInfo"]["homedir"], "/home/agentos");
}

fn python_guest_identity_uses_kernel_owned_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-identity-python");
    let cwd = temp_dir("guest-identity-python-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-identity-python");
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
                RootFilesystemEntry {
                    path: String::from("/workspace"),
                    kind: RootFilesystemEntryKind::Directory,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: None,
                    encoding: None,
                    target: None,
                    executable: false,
                },
                RootFilesystemEntry {
                    path: String::from("/workspace/identity.py"),
                    kind: RootFilesystemEntryKind::File,
                    mode: None,
                    uid: None,
                    gid: None,
                    content: Some(String::from(
                        r#"
import json
import os
from pathlib import Path

print(json.dumps({
    "env_user": os.environ.get("USER"),
    "env_home": os.environ.get("HOME"),
    "env_pwd": os.environ.get("PWD"),
    "env_shell": os.environ.get("SHELL"),
    "env_path": os.environ.get("PATH"),
    "custom_env": os.environ.get("EXEC_REVIEW"),
    "internal_keys": sorted([
        key for key in os.environ
        if key.startswith("AGENTOS_") or key.startswith("NODE_SYNC_RPC_")
    ]),
    "path_home": str(Path.home()),
}))
"#,
                    )),
                    encoding: Some(RootFilesystemEntryEncoding::Utf8),
                    target: None,
                    executable: false,
                },
            ],
        },
    );

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            support::wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: String::from("proc-python-identity"),
                command: None,
                runtime: Some(GuestRuntimeKind::Python),
                entrypoint: Some(String::from("/workspace/identity.py")),
                args: Vec::new(),
                env: HashMap::from([(String::from("EXEC_REVIEW"), String::from("visible"))]),
                cwd: None,
                wasm_permission_tier: None,
                wasm_backend: None,
            }),
        ))
        .expect("start Python identity execution");
    assert!(matches!(
        result.response.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));

    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-identity",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from Python identity execution: {stderr}"
    );

    let parsed = parse_json_stdout(&stdout);
    assert_eq!(parsed["env_user"], "agentos");
    assert_eq!(parsed["env_home"], "/home/agentos");
    assert_eq!(parsed["env_pwd"], "/");
    assert_eq!(parsed["env_shell"], "/bin/sh");
    assert_eq!(parsed["env_path"], DEFAULT_GUEST_PATH_ENV);
    assert_eq!(parsed["custom_env"], "visible");
    assert_eq!(parsed["internal_keys"], Value::Array(Vec::new()));
    assert_eq!(parsed["path_home"], "/home/agentos");
}

fn wasm_guest_identity_commands_use_kernel_owned_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-identity-wasm");
    let cwd = temp_dir("guest-identity-wasm-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-identity-wasm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let wasm_bytes = wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $getid_t (func (param i32) (result i32)))
  (type $getpwuid_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "host_user" "getuid" (func $getuid (type $getid_t)))
  (import "host_user" "getgid" (func $getgid (type $getid_t)))
  (import "host_user" "getpwuid" (func $getpwuid (type $getpwuid_t)))
  (memory (export "memory") 1)
  (func $assert_zero (param $errno i32)
    local.get $errno
    i32.eqz
    if
    else
      unreachable
    end)
  (func $assert_value (param $value i32) (param $expected i32)
    local.get $value
    local.get $expected
    i32.eq
    if
    else
      unreachable
    end)
  (func $write_stdout (param $ptr i32) (param $len i32)
    i32.const 16
    local.get $ptr
    i32.store
    i32.const 20
    local.get $len
    i32.store
    i32.const 1
    i32.const 16
    i32.const 1
    i32.const 24
    call $fd_write
    call $assert_zero)
  (func $_start (export "_start")
    i32.const 0
    call $getuid
    call $assert_zero
    i32.const 0
    i32.load
    i32.const 1000
    call $assert_value

    i32.const 4
    call $getgid
    call $assert_zero
    i32.const 4
    i32.load
    i32.const 1000
    call $assert_value

    i32.const 0
    i32.load
    i32.const 128
    i32.const 1
    i32.const 8
    call $getpwuid
    i32.const 68
    call $assert_value
    i32.const 8
    i32.load
    i32.const 42
    call $assert_value

    i32.const 0
    i32.load
    i32.const 128
    i32.const 256
    i32.const 8
    call $getpwuid
    call $assert_zero

    i32.const 128
    i32.const 8
    i32.load
    call $write_stdout
  ))
"#,
    )
    .expect("compile wasm identity fixture");
    let wasm_path = std::path::Path::new("/identity.wasm");
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![executable_wasm_root_entry(
                wasm_path.to_str().unwrap(),
                &wasm_bytes,
            )],
        },
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-identity",
        GuestRuntimeKind::WebAssembly,
        wasm_path,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-identity",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from wasm identity execution: {stderr}"
    );
    assert_eq!(stdout, "agentos:x:1000:1000::/home/agentos:/bin/sh");
}

fn wasm_guest_created_pty_uses_live_bounded_kernel_state() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-created-pty-wasm");
    let cwd = temp_dir("guest-created-pty-wasm-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-created-pty-wasm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let wasm_bytes = wat::parse_str(
        r#"
(module
  (type $pty_open_t (func (param i32 i32) (result i32)))
  (type $isatty_t (func (param i32) (result i32)))
  (type $get_size_t (func (param i32 i32 i32) (result i32)))
  (type $fd_close_t (func (param i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $proc_exit_t (func (param i32)))
  (import "host_process" "pty_open" (func $pty_open (type $pty_open_t)))
  (import "host_tty" "isatty" (func $isatty (type $isatty_t)))
  (import "host_tty" "get_size" (func $get_size (type $get_size_t)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (type $fd_close_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (type $proc_exit_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "pty:kernel-bounded\n")
  (func $fail (param $code i32)
    local.get $code
    call $proc_exit
    unreachable)
  (func $_start (export "_start")
    ;; The first guest-created PTY must be backed by live kernel descriptors.
    i32.const 0
    i32.const 4
    call $pty_open
    i32.eqz
    if
    else
      i32.const 41
      call $fail
    end
    i32.const 0
    i32.load
    i32.const 4
    i32.load
    i32.eq
    if
      i32.const 42
      call $fail
    end
    i32.const 0
    i32.load
    call $isatty
    i32.const 1
    i32.ne
    if
      i32.const 43
      call $fail
    end
    i32.const 4
    i32.load
    call $isatty
    i32.const 1
    i32.ne
    if
      i32.const 44
      call $fail
    end
    i32.const 0
    i32.load
    i32.const 8
    i32.const 10
    call $get_size
    i32.eqz
    if
    else
      i32.const 45
      call $fail
    end
    i32.const 8
    i32.load16_u
    i32.const 80
    i32.ne
    if
      i32.const 46
      call $fail
    end
    i32.const 10
    i32.load16_u
    i32.const 24
    i32.ne
    if
      i32.const 47
      call $fail
    end

    ;; This VM admits exactly one PTY. A second request must fail atomically
    ;; with EAGAIN and leave both guest output pointers untouched.
    i32.const 12
    i32.const 287454020
    i32.store
    i32.const 16
    i32.const 1432778632
    i32.store
    i32.const 12
    i32.const 16
    call $pty_open
    i32.const 6
    i32.ne
    if
      i32.const 48
      call $fail
    end
    i32.const 12
    i32.load
    i32.const 287454020
    i32.ne
    if
      i32.const 49
      call $fail
    end
    i32.const 16
    i32.load
    i32.const 1432778632
    i32.ne
    if
      i32.const 50
      call $fail
    end

    i32.const 0
    i32.load
    call $fd_close
    i32.eqz
    if
    else
      i32.const 51
      call $fail
    end
    i32.const 4
    i32.load
    call $fd_close
    i32.eqz
    if
    else
      i32.const 52
      call $fail
    end
    i32.const 24
    i32.const 64
    i32.store
    i32.const 28
    i32.const 19
    i32.store
    i32.const 1
    i32.const 24
    i32.const 1
    i32.const 32
    call $fd_write
    i32.eqz
    if
    else
      i32.const 53
      call $fail
    end))
"#,
    )
    .expect("compile guest-created PTY fixture");
    let wasm_path = std::path::Path::new("/guest-pty.wasm");
    let vm_id = create_vm_with_root_filesystem_and_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![executable_wasm_root_entry(
                wasm_path.to_str().unwrap(),
                &wasm_bytes,
            )],
        },
        HashMap::from([(String::from("resource.max_ptys"), String::from("1"))]),
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-guest-pty",
        GuestRuntimeKind::WebAssembly,
        wasm_path,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-guest-pty",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected PTY stderr: {stderr}");
    assert_eq!(stdout, "pty:kernel-bounded\n");
}

fn wasm_guest_env_filters_internal_control_vars_and_uses_kernel_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-env-wasm");
    let cwd = temp_dir("guest-env-wasm-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-env-wasm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let wasm_bytes = wat::parse_str(
            r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $environ_sizes_get_t (func (param i32 i32) (result i32)))
  (type $environ_get_t (func (param i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "environ_sizes_get" (func $environ_sizes_get (type $environ_sizes_get_t)))
  (import "wasi_snapshot_preview1" "environ_get" (func $environ_get (type $environ_get_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "\n")
  (func $assert_zero (param $errno i32)
    local.get $errno
    i32.eqz
    if
    else
      unreachable
    end)
  (func $strlen (param $ptr i32) (result i32)
    (local $len i32)
    (loop $loop
      local.get $ptr
      local.get $len
      i32.add
      i32.load8_u
      i32.eqz
      if
        local.get $len
        return
      end
      local.get $len
      i32.const 1
      i32.add
      local.set $len
      br $loop)
    i32.const 0)
  (func $write_buffer (param $ptr i32) (param $len i32)
    i32.const 0
    local.get $ptr
    i32.store
    i32.const 4
    local.get $len
    i32.store
    i32.const 1
    i32.const 0
    i32.const 1
    i32.const 8
    call $fd_write
    call $assert_zero)
  (func $_start (export "_start")
    (local $count i32)
    (local $index i32)
    (local $ptr i32)
    i32.const 256
    i32.const 260
    call $environ_sizes_get
    call $assert_zero
    i32.const 256
    i32.load
    local.set $count
    i32.const 512
    i32.const 1024
    call $environ_get
    call $assert_zero
    (loop $env_loop
      local.get $index
      local.get $count
      i32.lt_u
      if
        i32.const 512
        local.get $index
        i32.const 4
        i32.mul
        i32.add
        i32.load
        local.set $ptr
        local.get $ptr
        local.get $ptr
        call $strlen
        call $write_buffer
        i32.const 16
        i32.const 1
        call $write_buffer
        local.get $index
        i32.const 1
        i32.add
        local.set $index
        br $env_loop
      end)))
"#,
        )
        .expect("compile wasm env fixture");
    let wasm_path = std::path::Path::new("/env.wasm");
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![executable_wasm_root_entry(
                wasm_path.to_str().unwrap(),
                &wasm_bytes,
            )],
        },
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-env",
        GuestRuntimeKind::WebAssembly,
        wasm_path,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-env",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from wasm env execution: {stderr}"
    );

    let env = parse_env_stdout(&stdout);
    let leaked_internal = env
        .keys()
        .filter(|key| key.starts_with("AGENTOS_") || key.starts_with("NODE_SYNC_RPC_"))
        .cloned()
        .collect::<BTreeSet<_>>();

    assert_eq!(env.get("HOME").map(String::as_str), Some("/home/agentos"));
    assert_eq!(env.get("USER").map(String::as_str), Some("agentos"));
    assert_eq!(
        env.get("PATH").map(String::as_str),
        Some(DEFAULT_GUEST_PATH_ENV)
    );
    assert!(
        leaked_internal.is_empty(),
        "unexpected internal env leakage: {leaked_internal:?}"
    );
}

fn wasm_preopens_and_rights_are_kernel_authoritative() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-preopen-wasm");
    let cwd = temp_dir("guest-preopen-wasm-cwd");
    let connection_id = authenticate_wire(&mut sidecar, "conn-guest-preopen-wasm");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let wasm_bytes = wat::parse_str(
            r#"
(module
  (type $fd_prestat_get_t (func (param i32 i32) (result i32)))
  (type $fd_prestat_dir_name_t (func (param i32 i32 i32) (result i32)))
  (type $fd_fdstat_get_t (func (param i32 i32) (result i32)))
  (type $fd_close_t (func (param i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_prestat_get" (func $fd_prestat_get (type $fd_prestat_get_t)))
  (import "wasi_snapshot_preview1" "fd_prestat_dir_name" (func $fd_prestat_dir_name (type $fd_prestat_dir_name_t)))
  (import "wasi_snapshot_preview1" "fd_fdstat_get" (func $fd_fdstat_get (type $fd_fdstat_get_t)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (type $fd_close_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 128) "preopen:kernel\n")
  (func $assert_zero (param $value i32)
    local.get $value
    i32.eqz
    if
    else
      unreachable
    end)
  (func $_start (export "_start")
    i32.const 3
    i32.const 0
    call $fd_prestat_get
    call $assert_zero
    i32.const 4
    i32.load
    i32.eqz
    if unreachable end

    i32.const 3
    i32.const 16
    i32.const 1
    call $fd_prestat_dir_name
    call $assert_zero
    i32.const 16
    i32.load8_u
    i32.const 47
    i32.ne
    if unreachable end

    i32.const 3
    i32.const 32
    call $fd_fdstat_get
    call $assert_zero
    i32.const 40
    i64.load
    i64.const 8192
    i64.and
    i64.eqz
    if unreachable end
    i32.const 40
    i64.load
    i64.const 64
    i64.and
    i64.eqz
    if unreachable end

    i32.const 3
    call $fd_close
    call $assert_zero
    ;; Closing the public Linux descriptor must not revoke wasi-libc's private
    ;; tagged capability root, but the public descriptor itself is now bad.
    i32.const 3
    i32.const 0
    call $fd_prestat_get
    i32.const 8
    i32.ne
    if unreachable end

    i32.const 96
    i32.const 128
    i32.store
    i32.const 100
    i32.const 15
    i32.store
    i32.const 1
    i32.const 96
    i32.const 1
    i32.const 104
    call $fd_write
    call $assert_zero))
"#,
        )
        .expect("compile hostile WASI preopen fixture");
    let wasm_path = std::path::Path::new("/preopen.wasm");
    let vm_id = create_vm_with_root_filesystem(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: false,
            lowers: Vec::new(),
            bootstrap_entries: vec![executable_wasm_root_entry(
                wasm_path.to_str().unwrap(),
                &wasm_bytes,
            )],
        },
    );

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-preopen",
        GuestRuntimeKind::WebAssembly,
        wasm_path,
        Vec::new(),
    );
    let (stdout, stderr, exit_code) = collect_guest_identity_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-preopen",
    );
    dispose_vm_and_close_session(&mut sidecar, &connection_id, &session_id, &vm_id);
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert_eq!(stdout, "preopen:kernel\n");
}

fn run_named_case(case_name: &str) {
    match case_name {
        "javascript" => javascript_guest_identity_uses_kernel_owned_defaults(),
        "python" => python_guest_identity_uses_kernel_owned_defaults(),
        "wasm_identity" => wasm_guest_identity_commands_use_kernel_owned_defaults(),
        "wasm_pty" => wasm_guest_created_pty_uses_live_bounded_kernel_state(),
        "wasm_env" => wasm_guest_env_filters_internal_control_vars_and_uses_kernel_defaults(),
        "wasm_preopen" => wasm_preopens_and_rights_are_kernel_authoritative(),
        other => panic!("unknown guest_identity case: {other}"),
    }
}

fn collect_guest_identity_process_output(
    sidecar: &mut agentos_vm::VmManager<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
) -> (String, String, i32) {
    collect_process_output_wire_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        Duration::from_secs(10),
    )
}

#[test]
fn guest_identity_cases() {
    let current_exe = std::env::current_exe().expect("current test binary path");

    for case_name in GUEST_IDENTITY_CASES {
        let backends: &[Option<&str>] = if case_name.starts_with("wasm_") {
            &[Some("v8"), Some("wasmtime")]
        } else {
            &[None]
        };
        for backend in backends {
            let mut command = Command::new(&current_exe);
            command
                .arg("--exact")
                .arg("__guest_identity_case_runner")
                .arg("--nocapture")
                .env("AGENTOS_GUEST_IDENTITY_CASE", case_name);
            if let Some(backend) = backend {
                command.env("AGENTOS_TEST_WASM_BACKEND", backend);
            }
            let status = command.status().unwrap_or_else(|error| {
                panic!("spawn guest_identity runner for {case_name}/{backend:?}: {error}")
            });

            assert!(
                status.success(),
                "guest_identity case {case_name}/{backend:?} failed with status {status}"
            );
        }
    }
}

#[test]
fn __guest_identity_case_runner() {
    let Ok(case_name) = std::env::var("AGENTOS_GUEST_IDENTITY_CASE") else {
        return;
    };

    run_named_case(&case_name);
}
