#![allow(dead_code)]

#[path = "../../../bridge/tests/support.rs"]
mod bridge_support;
#[path = "../../src/plugins/s3_common.rs"]
mod s3_common;

use agentos_native_sidecar::protocol::{
    DisposeReason, EventFrame, GuestRuntimeKind, OwnershipScope, RequestFrame, RequestId,
    RequestPayload, ResponseFrame,
};
use agentos_native_sidecar::{DispatchResult, NativeSidecar, NativeSidecarConfig};
pub use bridge_support::RecordingBridge;
#[allow(unused_imports)]
pub(crate) use s3_common::test_support::MockS3Server;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const TEST_AUTH_TOKEN: &str = "sidecar-test-token";
const MAX_COLLECTED_PROCESS_STREAM_BYTES: usize = 1024 * 1024;

pub fn acquire_sidecar_runtime_test_lock() {
    // No-op under cargo-nextest: each test runs in its own process, so the
    // process-global V8 platform, env, and compile cache are already isolated.
    // Previously an exclusive flock serialized runtime tests across binaries. See
    // CLAUDE.md > Testing.
}

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
        "agentos-native-sidecar-{name}-{}",
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
    acquire_sidecar_runtime_test_lock();
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

pub fn request(id: RequestId, ownership: OwnershipScope, payload: RequestPayload) -> RequestFrame {
    RequestFrame::new(id, ownership, payload)
}

pub fn wire_request(
    id: agentos_native_sidecar::wire::RequestId,
    ownership: agentos_native_sidecar::wire::OwnershipScope,
    payload: agentos_native_sidecar::wire::RequestPayload,
) -> agentos_native_sidecar::wire::RequestFrame {
    agentos_native_sidecar::wire::RequestFrame {
        schema: agentos_native_sidecar::wire::protocol_schema(),
        request_id: id,
        ownership,
        payload,
    }
}

pub fn wire_connection(connection_id: &str) -> agentos_native_sidecar::wire::OwnershipScope {
    agentos_native_sidecar::wire::OwnershipScope::ConnectionOwnership(
        agentos_native_sidecar::wire::ConnectionOwnership {
            connection_id: connection_id.to_owned(),
        },
    )
}

pub fn wire_session(
    connection_id: &str,
    session_id: &str,
) -> agentos_native_sidecar::wire::OwnershipScope {
    agentos_native_sidecar::wire::OwnershipScope::SessionOwnership(
        agentos_native_sidecar::wire::SessionOwnership {
            connection_id: connection_id.to_owned(),
            session_id: session_id.to_owned(),
        },
    )
}

pub fn wire_vm(
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
) -> agentos_native_sidecar::wire::OwnershipScope {
    agentos_native_sidecar::wire::OwnershipScope::VmOwnership(
        agentos_native_sidecar::wire::VmOwnership {
            connection_id: connection_id.to_owned(),
            session_id: session_id.to_owned(),
            vm_id: vm_id.to_owned(),
        },
    )
}

pub fn wire_permissions_allow_all() -> agentos_native_sidecar::wire::PermissionsPolicy {
    agentos_native_sidecar::wire::PermissionsPolicy {
        fs: Some(
            agentos_native_sidecar::wire::FsPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
        network: Some(
            agentos_native_sidecar::wire::PatternPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
        child_process: Some(
            agentos_native_sidecar::wire::PatternPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
        process: Some(
            agentos_native_sidecar::wire::PatternPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
        env: Some(
            agentos_native_sidecar::wire::PatternPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
        binding: Some(
            agentos_native_sidecar::wire::PatternPermissionScope::PermissionMode(
                agentos_native_sidecar::wire::PermissionMode::Allow,
            ),
        ),
    }
}

pub fn authenticate_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_hint: &str,
) -> String {
    let result = authenticate_wire_with_token(sidecar, 1, connection_hint, TEST_AUTH_TOKEN);

    match result.response.payload {
        agentos_native_sidecar::wire::ResponsePayload::AuthenticatedResponse(response) => {
            assert_eq!(
                result.response.ownership,
                wire_connection(&response.connection_id)
            );
            response.connection_id
        }
        other => panic!("unexpected wire auth response: {other:?}"),
    }
}

pub fn authenticate_wire_with_token(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: agentos_native_sidecar::wire::RequestId,
    connection_hint: &str,
    auth_token: &str,
) -> agentos_native_sidecar::wire::WireDispatchResult {
    sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_connection(connection_hint),
            agentos_native_sidecar::wire::RequestPayload::AuthenticateRequest(
                agentos_native_sidecar::wire::AuthenticateRequest {
                    client_name: String::from("sidecar-tests"),
                    auth_token: auth_token.to_owned(),
                    protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
                    bridge_version: agentos_bridge::bridge_contract().version,
                },
            ),
        ))
        .expect("authenticate connection through wire")
}

pub fn open_session_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: agentos_native_sidecar::wire::RequestId,
    connection_id: &str,
) -> String {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_connection(connection_id),
            agentos_native_sidecar::wire::RequestPayload::OpenSessionRequest(
                agentos_native_sidecar::wire::OpenSessionRequest {
                    placement:
                        agentos_native_sidecar::wire::SidecarPlacement::SidecarPlacementShared(
                            agentos_native_sidecar::wire::SidecarPlacementShared { pool: None },
                        ),
                    metadata: HashMap::new(),
                },
            ),
        ))
        .expect("open sidecar session through wire");

    match result.response.payload {
        agentos_native_sidecar::wire::ResponsePayload::SessionOpenedResponse(response) => {
            response.session_id
        }
        other => panic!("unexpected wire session response: {other:?}"),
    }
}

pub fn create_vm_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: agentos_native_sidecar::wire::RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: agentos_native_sidecar::wire::GuestRuntimeKind,
    cwd: &Path,
) -> (String, agentos_native_sidecar::wire::WireDispatchResult) {
    create_vm_wire_with_metadata(
        sidecar,
        request_id,
        connection_id,
        session_id,
        runtime,
        cwd,
        HashMap::new(),
    )
}

pub fn create_vm_wire_with_metadata(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: agentos_native_sidecar::wire::RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: agentos_native_sidecar::wire::GuestRuntimeKind,
    cwd: &Path,
    mut metadata: HashMap<String, String>,
) -> (String, agentos_native_sidecar::wire::WireDispatchResult) {
    metadata
        .entry(String::from("cwd"))
        .or_insert_with(|| cwd.to_string_lossy().into_owned());

    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_session(connection_id, session_id),
            agentos_native_sidecar::wire::RequestPayload::CreateVmRequest(
                agentos_native_sidecar::wire::CreateVmRequest::legacy_test_config(
                    runtime,
                    metadata,
                    agentos_native_sidecar::wire::RootFilesystemDescriptor {
                        mode: agentos_native_sidecar::wire::RootFilesystemMode::Ephemeral,
                        disable_default_base_layer: false,
                        lowers: Vec::new(),
                        bootstrap_entries: Vec::new(),
                    },
                    Some(wire_permissions_allow_all()),
                ),
            ),
        ))
        .expect("create sidecar VM through wire");

    let vm_id = match &result.response.payload {
        agentos_native_sidecar::wire::ResponsePayload::VmCreatedResponse(response) => {
            response.vm_id.clone()
        }
        other => panic!("unexpected wire vm create response: {other:?}"),
    };
    (vm_id, result)
}

#[allow(clippy::too_many_arguments)]
pub fn execute_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: agentos_native_sidecar::wire::RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    runtime: agentos_native_sidecar::wire::GuestRuntimeKind,
    entrypoint: &Path,
    args: Vec<String>,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            agentos_native_sidecar::wire::RequestPayload::ExecuteRequest(
                agentos_native_sidecar::wire::ExecuteRequest {
                    process_id: process_id.to_owned(),
                    command: None,
                    runtime: Some(runtime),
                    entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                    args,
                    env: HashMap::new(),
                    cwd: None,
                    wasm_permission_tier: None,
                },
            ),
        ))
        .expect("start sidecar execution through wire");

    match result.response.payload {
        agentos_native_sidecar::wire::ResponsePayload::ProcessStartedResponse(response) => {
            assert_eq!(response.process_id, process_id);
        }
        other => panic!("unexpected wire execute response: {other:?}"),
    }
}

#[derive(Debug)]
pub struct ProcessOutputTimeout {
    pub stdout: String,
    pub stderr: String,
}

pub fn try_collect_process_output_wire_with_timeout(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    timeout: Duration,
) -> Result<(String, String, i32), ProcessOutputTimeout> {
    let ownership = wire_session(connection_id, session_id);
    let deadline = Instant::now() + timeout;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit = None;

    loop {
        let event = sidecar
            .poll_event_wire_blocking(&ownership, Duration::from_millis(100))
            .expect("poll sidecar wire event");
        if let Some(event) = event {
            assert_eq!(event.ownership, wire_vm(connection_id, session_id, vm_id));

            match event.payload {
                agentos_native_sidecar::wire::EventPayload::ProcessOutputEvent(output) => {
                    if output.process_id == process_id {
                        match output.channel {
                            agentos_native_sidecar::wire::StreamChannel::Stdout => {
                                append_process_stream_chunk(
                                    &mut stdout,
                                    &output.chunk,
                                    process_id,
                                    "stdout",
                                );
                            }
                            agentos_native_sidecar::wire::StreamChannel::Stderr => {
                                append_process_stream_chunk(
                                    &mut stderr,
                                    &output.chunk,
                                    process_id,
                                    "stderr",
                                );
                            }
                        }
                    }
                }
                agentos_native_sidecar::wire::EventPayload::ProcessExitedEvent(exited)
                    if exited.process_id == process_id =>
                {
                    exit = Some((exited.exit_code, Instant::now()));
                }
                agentos_native_sidecar::wire::EventPayload::ProcessExitedEvent(_)
                | agentos_native_sidecar::wire::EventPayload::VmLifecycleEvent(_)
                | agentos_native_sidecar::wire::EventPayload::StructuredEvent(_)
                | agentos_native_sidecar::wire::EventPayload::ExecutionOutputEvent(_)
                | agentos_native_sidecar::wire::EventPayload::ExecutionCompletedEvent(_)
                | agentos_native_sidecar::wire::EventPayload::ExtEnvelope(_) => {}
            }
        }

        if let Some((exit_code, seen_at)) = exit {
            if Instant::now().duration_since(seen_at) >= Duration::from_millis(200) {
                return Ok((
                    process_stream_to_string(&stdout),
                    process_stream_to_string(&stderr),
                    exit_code,
                ));
            }
        }

        if Instant::now() >= deadline {
            return Err(ProcessOutputTimeout {
                stdout: process_stream_to_string(&stdout),
                stderr: process_stream_to_string(&stderr),
            });
        }
    }
}

pub fn collect_process_output_wire_with_timeout(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    timeout: Duration,
) -> (String, String, i32) {
    try_collect_process_output_wire_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        timeout,
    )
    .unwrap_or_else(|output| {
        panic!(
            "timed out waiting for wire process events; stdout bytes: {}; stderr bytes: {}",
            output.stdout.len(),
            output.stderr.len()
        )
    })
}

pub fn authenticate(sidecar: &mut NativeSidecar<RecordingBridge>, connection_hint: &str) -> String {
    authenticate_wire(sidecar, connection_hint)
}

pub fn authenticate_with_token(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: RequestId,
    connection_hint: &str,
    auth_token: &str,
) -> DispatchResult {
    let result = authenticate_wire_with_token(sidecar, request_id, connection_hint, auth_token);
    dispatch_result_from_wire(result)
}

pub fn open_session(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
) -> String {
    open_session_wire(sidecar, request_id, connection_id)
}

pub fn create_vm(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: RequestId,
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
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    runtime: GuestRuntimeKind,
    cwd: &Path,
    metadata: BTreeMap<String, String>,
) -> (String, DispatchResult) {
    let (vm_id, result) = create_vm_wire_with_metadata(
        sidecar,
        request_id,
        connection_id,
        session_id,
        wire_runtime_kind(runtime),
        cwd,
        metadata.into_iter().collect(),
    );
    (vm_id, dispatch_result_from_wire(result))
}

#[allow(clippy::too_many_arguments)]
pub fn execute(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: RequestId,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    runtime: GuestRuntimeKind,
    entrypoint: &Path,
    args: Vec<String>,
) {
    execute_wire(
        sidecar,
        request_id,
        connection_id,
        session_id,
        vm_id,
        process_id,
        wire_runtime_kind(runtime),
        entrypoint,
        args,
    );
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
    collect_process_output_wire_with_timeout(
        sidecar,
        connection_id,
        session_id,
        vm_id,
        process_id,
        timeout,
    )
}

fn dispatch_result_from_wire(
    result: agentos_native_sidecar::wire::WireDispatchResult,
) -> DispatchResult {
    DispatchResult {
        response: response_frame_from_wire(result.response),
        events: result
            .events
            .into_iter()
            .map(event_frame_from_wire)
            .collect(),
    }
}

fn response_frame_from_wire(
    response: agentos_native_sidecar::wire::ResponseFrame,
) -> ResponseFrame {
    match agentos_native_sidecar::protocol::from_generated_protocol_frame(
        agentos_native_sidecar::wire::ProtocolFrame::ResponseFrame(response),
    )
    .expect("convert wire response frame to compatibility frame")
    {
        agentos_native_sidecar::protocol::ProtocolFrame::Response(response) => response,
        other => panic!("unexpected compatibility response conversion: {other:?}"),
    }
}

fn event_frame_from_wire(event: agentos_native_sidecar::wire::EventFrame) -> EventFrame {
    match agentos_native_sidecar::protocol::from_generated_protocol_frame(
        agentos_native_sidecar::wire::ProtocolFrame::EventFrame(event),
    )
    .expect("convert wire event frame to compatibility frame")
    {
        agentos_native_sidecar::protocol::ProtocolFrame::Event(event) => event,
        other => panic!("unexpected compatibility event conversion: {other:?}"),
    }
}

fn wire_runtime_kind(runtime: GuestRuntimeKind) -> agentos_native_sidecar::wire::GuestRuntimeKind {
    match runtime {
        GuestRuntimeKind::JavaScript => agentos_native_sidecar::wire::GuestRuntimeKind::JavaScript,
        GuestRuntimeKind::Python => agentos_native_sidecar::wire::GuestRuntimeKind::Python,
        GuestRuntimeKind::WebAssembly => {
            agentos_native_sidecar::wire::GuestRuntimeKind::WebAssembly
        }
    }
}

fn append_process_stream_chunk(
    stream: &mut Vec<u8>,
    chunk: &[u8],
    process_id: &str,
    stream_name: &str,
) {
    assert!(
        stream.len().saturating_add(chunk.len()) <= MAX_COLLECTED_PROCESS_STREAM_BYTES,
        "process {process_id} {stream_name} exceeded {MAX_COLLECTED_PROCESS_STREAM_BYTES} bytes"
    );
    stream.extend_from_slice(chunk);
}

fn process_stream_to_string(stream: &[u8]) -> String {
    String::from_utf8_lossy(stream).into_owned()
}

#[test]
fn collect_process_output_stream_append_is_bounded() {
    let mut stream = Vec::new();
    append_process_stream_chunk(&mut stream, &[b'a'; 16], "proc-limit", "stdout");
    assert_eq!(stream.len(), 16);

    let overflow = std::panic::catch_unwind(|| {
        let mut stream = vec![b'a'; MAX_COLLECTED_PROCESS_STREAM_BYTES];
        append_process_stream_chunk(&mut stream, b"!", "proc-limit", "stdout");
    });
    assert!(
        overflow.is_err(),
        "oversized process output should fail the shared test harness"
    );
}

pub fn dispose_vm_and_close_session(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
) {
    sidecar
        .dispose_vm_internal_blocking(connection_id, session_id, vm_id, DisposeReason::Requested)
        .expect("dispose sidecar VM");
    sidecar
        .close_session_blocking(connection_id, session_id)
        .expect("close sidecar session");
    sidecar
        .remove_connection_blocking(connection_id)
        .expect("remove sidecar connection");
}

pub fn dispose_vm_and_close_session_wire(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
) {
    let result = sidecar
        .dispatch_wire_blocking(wire_request(
            900,
            wire_vm(connection_id, session_id, vm_id),
            agentos_native_sidecar::wire::RequestPayload::DisposeVmRequest(
                agentos_native_sidecar::wire::DisposeVmRequest {
                    reason: agentos_native_sidecar::wire::DisposeReason::Requested,
                },
            ),
        ))
        .expect("dispose sidecar VM through wire");

    match result.response.payload {
        agentos_native_sidecar::wire::ResponsePayload::VmDisposedResponse(response) => {
            assert_eq!(response.vm_id, vm_id);
        }
        other => panic!("unexpected wire vm dispose response: {other:?}"),
    }
    sidecar
        .close_session_blocking(connection_id, session_id)
        .expect("close sidecar session");
    sidecar
        .remove_connection_blocking(connection_id)
        .expect("remove sidecar connection");
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
  (data (i32.const 32) "signal:ready\n")
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
    (i32.store (i32.const 4) (i32.const 13))
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
