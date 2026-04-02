mod support;

use agent_os_sidecar::protocol::{
    GuestRuntimeKind, OwnershipScope, RequestPayload, ResponsePayload, WriteStdinRequest,
};
use agent_os_sidecar::{NativeSidecar, NativeSidecarConfig};
use serde_json::Value;
use std::collections::BTreeMap;
use support::{
    assert_node_available, authenticate, collect_process_output, create_vm,
    create_vm_with_metadata, execute, open_session, request, temp_dir, write_fixture,
    RecordingBridge, TEST_AUTH_TOKEN,
};

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
    marker: process.env.AGENT_OS_ALLOWED ?? null,
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

  const fs = require('fs');
  try {
    fs.readFileSync('/proc/self/environ', 'utf8');
    result.procEnviron = 'unexpected';
  } catch (error) {
    result.procEnviron = { code: error.code ?? null, message: error.message };
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
        BTreeMap::from([(
            String::from("env.AGENT_OS_ALLOWED"),
            String::from("present"),
        )]),
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
    assert_eq!(
        parsed["procEnviron"]["code"],
        Value::String(String::from("ERR_ACCESS_DENIED"))
    );
}

#[test]
fn vm_resource_limits_cap_active_processes_without_poisoning_followup_execs() {
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
