mod support;

use agent_os_sidecar::protocol::{
    CreateVmRequest, GuestRuntimeKind, OwnershipScope, RequestId, RequestPayload,
    ResponsePayload, RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm, execute, new_sidecar,
    open_session, request, temp_dir,
};

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
