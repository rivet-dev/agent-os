//! Regression guard: durable `session/update`s must stream live mid-turn even
//! when the agent emitted a message chunk first.
//!
//! Original bug: `DurableUpdateSink::handle_notification` buffered every
//! non-message update (`tool_call`, `tool_call_update`, `plan`, ...) whenever a
//! message chunk had already opened a completion buffer, and only committed the
//! buffer at the next message boundary. An agent that streams any text before
//! calling a tool therefore withheld the whole durable stream until the
//! *post-tool* message arrived, so a `tool_call_update { in_progress }` for a
//! long-running tool reached the host only when the tool finished — and a caller
//! that waits for that boundary before cancelling never saw a live turn to
//! cancel.
//!
//! The adapter here reproduces exactly that ordering: one `agent_message_chunk`,
//! then the tool updates, then a hold, then the prompt response. With live
//! delivery the tool update reaches the event sink during the hold; with the bug
//! it arrives only when the prompt resolves.
//!
//! A separate test file keeps this guard standalone (see the note in
//! `acp_request_timeout.rs`); it installs its own `EventSinkTransport`, which
//! an in-process `NativeSidecar` otherwise does not have.

#[path = "support/bridge.rs"]
mod bridge_support;

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agentos_native_sidecar::protocol::EventPayload;
use agentos_native_sidecar::wire::{
    event_frame_to_compat, AuthenticateRequest, ConfigureVmRequest, ConnectionOwnership,
    CreateVmRequest, ExtEnvelope, GuestRuntimeKind, OpenSessionRequest, OwnershipScope,
    PackageDescriptor, RequestFrame, RequestPayload, ResponsePayload, SessionOwnership,
    SidecarPlacement, SidecarPlacementShared, VmOwnership,
};
use agentos_native_sidecar::{
    EventSinkTransport, NativeSidecar, NativeSidecarConfig, SidecarError,
};
use agentos_protocol::generated::v1::{
    AcpDurableEvent, AcpEvent, AcpOpenSessionRequest, AcpPromptRequest, AcpRequest, AcpResponse,
};
use agentos_protocol::ACP_EXTENSION_NAMESPACE;
use agentos_vm_config as vm_config;
use bridge_support::RecordingBridge;

/// How long the adapter holds the prompt open after producing the tool updates.
const ADAPTER_HOLD_MS: u64 = 2_000;

#[test]
fn tool_updates_stream_live_after_a_leading_message_chunk() {
    assert_node_available();
    let mut sidecar = new_sidecar("live-updates");
    let sink = Arc::new(RecordingEventSink::default());
    sidecar.set_event_transport(sink.clone());

    let connection_id = authenticate(&mut sidecar);
    let session_id = open_session(&mut sidecar, &connection_id);
    let cwd = temp_dir("live-updates-cwd");
    fs::write(cwd.join(ADAPTER_FILE), adapter_script()).expect("write adapter script");
    let vm_id = create_vm(&mut sidecar, &connection_id, &session_id, &cwd);

    let opened = dispatch_acp(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        AcpRequest::AcpOpenSessionRequest(AcpOpenSessionRequest {
            session_id: Some(String::from("live-session")),
            agent: String::from("pi"),
            cwd: Some(String::from("/home/agentos")),
            additional_directories: None,
            env: None,
            mcp_servers: None,
            permission_policy: None,
            skip_os_instructions: Some(true),
            additional_instructions: None,
        }),
    );
    assert!(
        matches!(opened, AcpResponse::AcpOpenSessionResponse(_)),
        "expected the mock ACP adapter to open a session, got: {opened:?}"
    );

    sink.reset();
    let started = Instant::now();
    let response = dispatch_acp(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        AcpRequest::AcpPromptRequest(AcpPromptRequest {
            session_id: Some(String::from("live-session")),
            idempotency_key: None,
            content: String::from(r#"[{"type":"text","text":"run the tool"}]"#),
        }),
    );
    let resolved = started.elapsed();
    assert!(
        matches!(response, AcpResponse::AcpPromptResponse(_)),
        "expected the prompt to complete, got: {response:?}"
    );
    assert!(
        resolved >= Duration::from_millis(ADAPTER_HOLD_MS),
        "the adapter must really hold the turn open, otherwise there is no \
         mid-turn window to observe; turn took {resolved:?}"
    );

    let in_progress = sink
        .first_durable_update_containing("\"in_progress\"")
        .expect(
            "the tool_call_update { in_progress } must reach the host at all — no durable \
             session update carrying it was emitted",
        );
    assert!(
        in_progress < Duration::from_millis(ADAPTER_HOLD_MS / 2),
        "BUG: tool_call_update {{ in_progress }} arrived after {in_progress:?}, i.e. it was \
         held in the message-completion buffer until the turn ended (turn: {resolved:?}) \
         instead of streaming when the adapter produced it",
    );
}

#[derive(Default)]
struct RecordingEventSink {
    started: Mutex<Option<Instant>>,
    events: Mutex<Vec<(Duration, AcpEvent)>>,
}

impl RecordingEventSink {
    fn reset(&self) {
        *self.started.lock().expect("sink clock") = Some(Instant::now());
        self.events.lock().expect("sink events").clear();
    }

    /// Elapsed time from `reset` to the first durable session update whose JSON
    /// contains `needle`.
    fn first_durable_update_containing(&self, needle: &str) -> Option<Duration> {
        self.events
            .lock()
            .expect("sink events")
            .iter()
            .find_map(|(at, event)| match event {
                AcpEvent::AcpDurableSessionEvent(durable) => match &durable.event {
                    AcpDurableEvent::AcpDurableSessionUpdate(update)
                        if update.update.contains(needle) =>
                    {
                        Some(*at)
                    }
                    _ => None,
                },
                _ => None,
            })
    }
}

impl EventSinkTransport for RecordingEventSink {
    fn emit_event(
        &self,
        event: agentos_native_sidecar::wire::EventFrame,
    ) -> Result<(), SidecarError> {
        let at = self
            .started
            .lock()
            .expect("sink clock")
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let frame = event_frame_to_compat(event)
            .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
        if let EventPayload::Ext(ExtEnvelope { namespace, payload }) = frame.payload {
            if namespace == ACP_EXTENSION_NAMESPACE {
                if let Ok(event) = serde_bare::from_slice::<AcpEvent>(&payload) {
                    self.events.lock().expect("sink events").push((at, event));
                }
            }
        }
        Ok(())
    }
}

const ADAPTER_FILE: &str = "live-updates-adapter.mjs";

/// Adapter that handshakes normally, then on `session/prompt` streams one
/// message chunk, the tool updates, holds the turn open, and finally responds.
fn adapter_script() -> String {
    format!(
        r#"#!/usr/bin/env node
import readline from "node:readline";

const lines = readline.createInterface({{ input: process.stdin }});
const send = (message) => console.log(JSON.stringify(message));
const update = (update) =>
  send({{
    jsonrpc: "2.0",
    method: "session/update",
    params: {{ sessionId: "adapter-session", update }},
  }});

for await (const line of lines) {{
  if (!line.trim()) continue;
  const message = JSON.parse(line);
  if (message.method === "initialize") {{
    send({{
      jsonrpc: "2.0",
      id: message.id,
      result: {{
        protocolVersion: message.params.protocolVersion,
        agentInfo: {{ name: "live-updates-acp-adapter" }},
        configOptions: []
      }}
    }});
  }} else if (message.method === "session/new") {{
    send({{
      jsonrpc: "2.0",
      id: message.id,
      result: {{
        sessionId: "adapter-session",
        modes: {{ currentModeId: "default", availableModes: [] }},
        models: {{
          currentModelId: "fast-model",
          availableModels: [{{ modelId: "fast-model", name: "Fast Model" }}]
        }}
      }}
    }});
  }} else if (message.method === "session/prompt") {{
    // A leading assistant chunk: this is what opens the completion buffer.
    update({{
      sessionUpdate: "agent_message_chunk",
      content: {{ type: "text", text: "banner\n" }}
    }});
    update({{
      sessionUpdate: "tool_call",
      toolCallId: "tool-1",
      title: "sleep",
      kind: "execute",
      status: "pending",
      rawInput: {{ command: "sleep 60" }}
    }});
    update({{
      sessionUpdate: "tool_call_update",
      toolCallId: "tool-1",
      title: "sleep",
      kind: "execute",
      status: "in_progress"
    }});
    // The tool is "running": nothing else is produced until it finishes.
    await new Promise((resolve) => setTimeout(resolve, {hold}));
    update({{
      sessionUpdate: "tool_call_update",
      toolCallId: "tool-1",
      status: "completed"
    }});
    send({{ jsonrpc: "2.0", id: message.id, result: {{ stopReason: "end_turn" }} }});
  }} else {{
    send({{
      jsonrpc: "2.0",
      id: message.id,
      error: {{ code: -32601, message: `unknown method ${{message.method}}` }}
    }});
  }}
}}
"#,
        hold = ADAPTER_HOLD_MS,
    )
}

fn dispatch_acp(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    request: AcpRequest,
) -> AcpResponse {
    let payload = serde_bare::to_vec(&request).expect("encode ACP request");
    let result = sidecar
        .dispatch_wire_blocking(RequestFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: connection_id.to_owned(),
                session_id: session_id.to_owned(),
                vm_id: vm_id.to_owned(),
            }),
            payload: RequestPayload::ExtEnvelope(ExtEnvelope {
                namespace: String::from(ACP_EXTENSION_NAMESPACE),
                payload,
            }),
        })
        .expect("dispatch ACP extension request");

    match result.response.payload {
        ResponsePayload::ExtEnvelope(envelope) => {
            assert_eq!(envelope.namespace, ACP_EXTENSION_NAMESPACE);
            serde_bare::from_slice(&envelope.payload).expect("decode ACP response")
        }
        ResponsePayload::RejectedResponse(rejected) => panic!(
            "ACP dispatch was rejected at the wire layer: code={} message={}",
            rejected.code, rejected.message
        ),
        other => panic!("unexpected sidecar response: {other:?}"),
    }
}

fn assert_node_available() {
    let output = Command::new("node")
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(output.status.success(), "node must be available");
}

fn new_sidecar(name: &str) -> NativeSidecar<RecordingBridge> {
    NativeSidecar::with_config_and_extensions(
        RecordingBridge::default(),
        NativeSidecarConfig {
            sidecar_id: format!("sidecar-{name}"),
            compile_cache_root: Some(temp_dir(name).join("cache")),
            ..NativeSidecarConfig::default()
        },
        agentos_sidecar_wrapper::extensions(),
    )
    .expect("create native sidecar")
}

fn authenticate(sidecar: &mut NativeSidecar<RecordingBridge>) -> String {
    let result = sidecar
        .dispatch_wire_blocking(RequestFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: 1,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: String::from("client"),
            }),
            payload: RequestPayload::AuthenticateRequest(AuthenticateRequest {
                client_name: String::from("acp-extension-live-updates"),
                auth_token: String::new(),
                protocol_version: agentos_native_sidecar::wire::PROTOCOL_VERSION,
                bridge_version: agentos_bridge::bridge_contract().version,
            }),
        })
        .expect("authenticate");
    match result.response.payload {
        ResponsePayload::AuthenticatedResponse(response) => response.connection_id,
        other => panic!("unexpected auth response: {other:?}"),
    }
}

fn open_session(sidecar: &mut NativeSidecar<RecordingBridge>, connection_id: &str) -> String {
    let result = sidecar
        .dispatch_wire_blocking(RequestFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: 2,
            ownership: OwnershipScope::ConnectionOwnership(ConnectionOwnership {
                connection_id: connection_id.to_owned(),
            }),
            payload: RequestPayload::OpenSessionRequest(OpenSessionRequest {
                placement: SidecarPlacement::SidecarPlacementShared(SidecarPlacementShared {
                    pool: None,
                }),
                metadata: HashMap::new(),
            }),
        })
        .expect("open session");
    match result.response.payload {
        ResponsePayload::SessionOpenedResponse(response) => response.session_id,
        other => panic!("unexpected session response: {other:?}"),
    }
}

fn create_vm(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    cwd: &Path,
) -> String {
    let result = sidecar
        .dispatch_wire_blocking(RequestFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: 3,
            ownership: OwnershipScope::SessionOwnership(SessionOwnership {
                connection_id: connection_id.to_owned(),
                session_id: session_id.to_owned(),
            }),
            payload: RequestPayload::CreateVmRequest(CreateVmRequest {
                runtime: GuestRuntimeKind::JavaScript,
                config: serde_json::to_string(&vm_config::CreateVmConfig {
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    database: Some(vm_config::VmSqliteDescriptor::SqliteFile {
                        path: cwd.join("agentos.sqlite").to_string_lossy().into_owned(),
                    }),
                    permissions: Some(allow_all_permissions()),
                    ..Default::default()
                })
                .expect("serialize create VM config"),
            }),
        })
        .expect("create VM");
    let vm_id = match result.response.payload {
        ResponsePayload::VmCreatedResponse(response) => response.vm_id,
        other => panic!("unexpected create VM response: {other:?}"),
    };
    configure_mock_agent_package(sidecar, connection_id, session_id, &vm_id, cwd);
    vm_id
}

fn configure_mock_agent_package(
    sidecar: &mut NativeSidecar<RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    cwd: &Path,
) {
    let script = fs::read_to_string(cwd.join(ADAPTER_FILE)).expect("read adapter script");
    let package_dir = cwd.join("packages").join("pi");
    let bin_dir = package_dir.join("bin");
    fs::create_dir_all(&bin_dir).expect("create mock agent bin dir");
    let manifest = serde_json::json!({
        "name": "pi",
        "version": "0.0.0",
        "agent": { "acpEntrypoint": "pi" },
    })
    .to_string();
    fs::write(package_dir.join("agentos-package.json"), manifest)
        .expect("write mock agent manifest");
    let command = bin_dir.join("pi");
    fs::write(&command, script).expect("write mock agent command");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755))
        .expect("make mock agent command executable");
    let result = sidecar
        .dispatch_wire_blocking(RequestFrame {
            schema: agentos_native_sidecar::wire::protocol_schema(),
            request_id: 30,
            ownership: OwnershipScope::VmOwnership(VmOwnership {
                connection_id: connection_id.to_owned(),
                session_id: session_id.to_owned(),
                vm_id: vm_id.to_owned(),
            }),
            payload: RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts: Vec::new(),
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: HashMap::new(),
                loopback_exempt_ports: Vec::new(),
                packages: vec![PackageDescriptor {
                    path: package_dir.to_string_lossy().into_owned(),
                }],
                packages_mount_at: String::from("/opt/agentos"),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        })
        .expect("configure mock ACP package");
    assert!(matches!(
        result.response.payload,
        ResponsePayload::VmConfiguredResponse(_)
    ));
}

fn allow_all_permissions() -> vm_config::PermissionsPolicy {
    vm_config::PermissionsPolicy {
        fs: Some(vm_config::FsPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        network: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        child_process: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        process: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        env: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
        binding: Some(vm_config::PatternPermissionScope::Mode(
            vm_config::PermissionMode::Allow,
        )),
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "agentos-sidecar-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("create temp dir");
    root
}
