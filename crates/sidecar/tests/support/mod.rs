#![allow(dead_code)]

#[path = "../../../bridge/tests/support.rs"]
mod bridge_support;

use agent_os_sidecar::protocol::{
    AuthenticateRequest, CreateVmRequest, EventPayload, ExecuteRequest, GuestRuntimeKind,
    OpenSessionRequest, OwnershipScope, ProcessOutputEvent, RequestFrame, RequestPayload,
    ResponsePayload, SidecarPlacement,
};
use agent_os_sidecar::{DispatchResult, NativeSidecar, NativeSidecarConfig};
pub use bridge_support::RecordingBridge;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const TEST_AUTH_TOKEN: &str = "sidecar-test-token";

pub fn assert_node_available() {
    let output = Command::new("node")
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(
        output.status.success(),
        "node must be available for native sidecar execution tests"
    );
}

pub fn temp_dir(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "agent-os-sidecar-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create temp dir");
    root
}

pub fn new_sidecar(name: &str) -> NativeSidecar<RecordingBridge> {
    new_sidecar_with_auth_token(name, TEST_AUTH_TOKEN)
}

pub fn new_sidecar_with_auth_token(
    name: &str,
    expected_auth_token: &str,
) -> NativeSidecar<RecordingBridge> {
    let root = temp_dir(name);
    NativeSidecar::with_config(
        RecordingBridge::default(),
        NativeSidecarConfig {
            sidecar_id: format!("sidecar-{name}"),
            compile_cache_root: Some(root.join("cache")),
            expected_auth_token: Some(expected_auth_token.to_owned()),
            ..NativeSidecarConfig::default()
        },
    )
    .expect("create native sidecar")
}

pub fn request(id: u64, ownership: OwnershipScope, payload: RequestPayload) -> RequestFrame {
    RequestFrame::new(id, ownership, payload)
}

pub fn authenticate(sidecar: &mut NativeSidecar<RecordingBridge>, connection_hint: &str) -> String {
    let result = authenticate_with_token(sidecar, 1, connection_hint, TEST_AUTH_TOKEN);

    match result.response.payload {
        ResponsePayload::Authenticated(response) => {
            assert_eq!(
                result.response.ownership,
                OwnershipScope::connection(&response.connection_id)
            );
            response.connection_id
        }
        other => panic!("unexpected auth response: {other:?}"),
    }
}

pub fn authenticate_with_token(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: u64,
    connection_hint: &str,
    auth_token: &str,
) -> DispatchResult {
    sidecar
        .dispatch(request(
            request_id,
            OwnershipScope::connection(connection_hint),
            RequestPayload::Authenticate(AuthenticateRequest {
                client_name: String::from("sidecar-tests"),
                auth_token: auth_token.to_owned(),
            }),
        ))
        .expect("authenticate connection")
}

pub fn open_session(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: u64,
    connection_id: &str,
) -> String {
    let result = sidecar
        .dispatch(request(
            request_id,
            OwnershipScope::connection(connection_id),
            RequestPayload::OpenSession(OpenSessionRequest {
                placement: SidecarPlacement::Shared { pool: None },
                metadata: BTreeMap::new(),
            }),
        ))
        .expect("open sidecar session");

    match result.response.payload {
        ResponsePayload::SessionOpened(response) => response.session_id,
        other => panic!("unexpected session response: {other:?}"),
    }
}

pub fn create_vm(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
) -> (String, DispatchResult) {
    create_vm_with_metadata(
        sidecar,
        request_id,
        connection_id,
        session_id,
        runtime,
        cwd,
        BTreeMap::new(),
    )
}

pub fn create_vm_with_metadata(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    mut metadata: BTreeMap<String, String>,
) -> (String, DispatchResult) {
    metadata
        .entry(String::from("cwd"))
        .or_insert_with(|| cwd.to_string_lossy().into_owned());

    let result = sidecar
        .dispatch(request(
            request_id,
            OwnershipScope::session(connection_id, session_id),
            RequestPayload::CreateVm(CreateVmRequest {
                runtime,
                metadata,
                root_filesystem: Default::default(),
            }),
        ))
        .expect("create sidecar VM");

    let vm_id = match &result.response.payload {
        ResponsePayload::VmCreated(response) => response.vm_id.clone(),
        other => panic!("unexpected vm create response: {other:?}"),
    };
    (vm_id, result)
}

pub fn execute(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: u64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    runtime: GuestRuntimeKind,
    entrypoint: &Path,
    args: Vec<String>,
) {
    let result = sidecar
        .dispatch(request(
            request_id,
            OwnershipScope::vm(connection_id, session_id, vm_id),
            RequestPayload::Execute(ExecuteRequest {
                process_id: process_id.to_owned(),
                runtime,
                entrypoint: entrypoint.to_string_lossy().into_owned(),
                args,
                env: BTreeMap::new(),
                cwd: None,
            }),
        ))
        .expect("start sidecar execution");

    match result.response.payload {
        ResponsePayload::ProcessStarted(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected execute response: {other:?}"),
    }
}

pub fn collect_process_output(
    sidecar: &mut NativeSidecar<RecordingBridge>,
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

pub fn collect_process_output_with_timeout(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    timeout: Duration,
) -> (String, String, i32) {
    let ownership = OwnershipScope::session(connection_id, session_id);
    let deadline = Instant::now() + timeout;
    let mut stdout = String::new();
    let mut stderr = String::new();

    loop {
        let event = sidecar
            .poll_event(&ownership, Duration::from_millis(100))
            .expect("poll sidecar event");
        let Some(event) = event else {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for process events"
            );
            continue;
        };

        assert_eq!(
            event.ownership,
            OwnershipScope::vm(connection_id, session_id, vm_id)
        );

        match event.payload {
            EventPayload::ProcessOutput(ProcessOutputEvent {
                process_id: event_process_id,
                channel,
                chunk,
            }) if event_process_id == process_id => match channel {
                agent_os_sidecar::protocol::StreamChannel::Stdout => stdout.push_str(&chunk),
                agent_os_sidecar::protocol::StreamChannel::Stderr => stderr.push_str(&chunk),
            },
            EventPayload::ProcessExited(exited) if exited.process_id == process_id => {
                return (stdout, stderr, exited.exit_code);
            }
            _ => {}
        }
    }
}

pub fn write_fixture(path: &Path, contents: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent");
    }
    fs::write(path, contents).expect("write fixture");
}

pub fn wasm_stdout_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "wasm:ready\n")
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 16))
    (i32.store (i32.const 4) (i32.const 11))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 32)
      )
    )
  )
)
"#,
    )
    .expect("compile wasm fixture")
}

pub fn wasm_signal_state_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $proc_sigaction_t (func (param i32 i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "host_process" "proc_sigaction" (func $proc_sigaction (type $proc_sigaction_t)))
  (memory (export "memory") 1)
  (data (i32.const 32) "signal-registered\n")
  (func $_start (export "_start")
    (drop
      (call $proc_sigaction
        (i32.const 2)
        (i32.const 2)
        (i32.const 16384)
        (i32.const 0)
        (i32.const 4660)
      )
    )
    (i32.store (i32.const 0) (i32.const 32))
    (i32.store (i32.const 4) (i32.const 18))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 24)
      )
    )
  )
)
"#,
    )
    .expect("compile signal-state wasm fixture")
}
