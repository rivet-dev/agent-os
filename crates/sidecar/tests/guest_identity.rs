mod support;

use agent_os_sidecar::protocol::{
    CreateVmRequest, GuestRuntimeKind, OwnershipScope, RequestId, RequestPayload, ResponsePayload,
    RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm, execute, new_sidecar,
    open_session, request, temp_dir,
};

const DEFAULT_GUEST_PATH_ENV: &str =
    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn create_vm_with_root_filesystem(
    sidecar: &mut agent_os_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &std::path::Path,
    root_filesystem: RootFilesystemDescriptor,
) -> String {
    let result = sidecar
        .dispatch_blocking(request(
            request_id,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime,
                metadata: BTreeMap::from([(
                    String::from("cwd"),
                    cwd.to_string_lossy().into_owned(),
                )]),
                root_filesystem,
                permissions: None,
            }),
        ))
        .expect("create sidecar VM");

    match result.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id,
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

#[test]
fn javascript_guest_identity_uses_kernel_owned_defaults() {
    let mut sidecar = new_sidecar("guest-identity-js");
    let cwd = temp_dir("guest-identity-js-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-guest-identity-js");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
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
    key.startsWith("AGENT_OS_") || key.startsWith("NODE_SYNC_RPC_")
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

    execute(
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

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-js-identity",
    );
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from JavaScript identity execution: {stderr}"
    );

    let parsed = parse_json_stdout(&stdout);
    assert_eq!(parsed["envUser"], "user");
    assert_eq!(parsed["envHome"], "/home/user");
    assert_eq!(parsed["envPwd"], "/");
    assert_eq!(parsed["envShell"], "/bin/sh");
    assert_eq!(parsed["envPath"], DEFAULT_GUEST_PATH_ENV);
    assert_eq!(parsed["internalKeys"], Value::Array(Vec::new()));
    assert_eq!(parsed["uid"], 1000);
    assert_eq!(parsed["gid"], 1000);
    assert_eq!(parsed["euid"], 1000);
    assert_eq!(parsed["egid"], 1000);
    assert_eq!(parsed["groups"], Value::Array(vec![Value::from(1000)]));
    assert_eq!(parsed["homedir"], "/home/user");
    assert_eq!(parsed["cwd"], "/");
    assert_eq!(parsed["userInfo"]["username"], "user");
    assert_eq!(parsed["userInfo"]["uid"], 1000);
    assert_eq!(parsed["userInfo"]["gid"], 1000);
    assert_eq!(parsed["userInfo"]["shell"], "/bin/sh");
    assert_eq!(parsed["userInfo"]["homedir"], "/home/user");
}

#[test]
fn python_guest_identity_uses_kernel_owned_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-identity-python");
    let cwd = temp_dir("guest-identity-python-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-guest-identity-python");
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
                    path: String::from("/workspace/identity.py"),
                    kind: RootFilesystemEntryKind::File,
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
    "internal_keys": sorted([
        key for key in os.environ
        if key.startswith("AGENT_OS_") or key.startswith("NODE_SYNC_RPC_")
    ]),
    "path_home": str(Path.home()),
}))
"#,
                    )),
                    encoding: Some(RootFilesystemEntryEncoding::Utf8),
                    executable: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        },
    );

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-identity",
        GuestRuntimeKind::Python,
        std::path::Path::new("/workspace/identity.py"),
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-python-identity",
    );
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from Python identity execution: {stderr}"
    );

    let parsed = parse_json_stdout(&stdout);
    assert_eq!(parsed["env_user"], "user");
    assert_eq!(parsed["env_home"], "/home/user");
    assert_eq!(parsed["env_pwd"], "/");
    assert_eq!(parsed["env_shell"], "/bin/sh");
    assert_eq!(parsed["env_path"], DEFAULT_GUEST_PATH_ENV);
    assert_eq!(parsed["internal_keys"], Value::Array(Vec::new()));
    assert_eq!(parsed["path_home"], "/home/user");
}

#[test]
fn wasm_guest_identity_commands_use_kernel_owned_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-identity-wasm");
    let cwd = temp_dir("guest-identity-wasm-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-guest-identity-wasm");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );

    let wasm_path = cwd.join("identity.wasm");
    fs::write(
        &wasm_path,
        wat::parse_str(
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
        .expect("compile wasm identity fixture"),
    )
    .expect("write wasm identity fixture");

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-identity",
        GuestRuntimeKind::WebAssembly,
        &wasm_path,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-identity",
    );
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from wasm identity execution: {stderr}"
    );
    assert_eq!(stdout, "user:x:1000:1000::/home/user:/bin/sh");
}

#[test]
fn wasm_guest_env_filters_internal_control_vars_and_uses_kernel_defaults() {
    assert_node_available();

    let mut sidecar = new_sidecar("guest-env-wasm");
    let cwd = temp_dir("guest-env-wasm-cwd");
    let connection_id = authenticate(&mut sidecar, "conn-guest-env-wasm");
    let session_id = open_session(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = create_vm(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );

    let wasm_path = cwd.join("env.wasm");
    fs::write(
        &wasm_path,
        wat::parse_str(
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
        .expect("compile wasm env fixture"),
    )
    .expect("write wasm env fixture");

    execute(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-env",
        GuestRuntimeKind::WebAssembly,
        &wasm_path,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "proc-wasm-env",
    );
    assert_eq!(exit_code, 0, "stderr:\n{stderr}");
    assert!(
        stderr.is_empty(),
        "unexpected stderr from wasm env execution: {stderr}"
    );

    let env = parse_env_stdout(&stdout);
    let leaked_internal = env
        .keys()
        .filter(|key| key.starts_with("AGENT_OS_") || key.starts_with("NODE_SYNC_RPC_"))
        .cloned()
        .collect::<BTreeSet<_>>();

    assert_eq!(env.get("HOME").map(String::as_str), Some("/home/user"));
    assert_eq!(env.get("USER").map(String::as_str), Some("user"));
    assert_eq!(env.get("PATH").map(String::as_str), Some(DEFAULT_GUEST_PATH_ENV));
    assert!(
        leaked_internal.is_empty(),
        "unexpected internal env leakage: {leaked_internal:?}"
    );
}
