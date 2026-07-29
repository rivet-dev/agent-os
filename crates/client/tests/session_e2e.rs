//! Agent session (ACP) e2e against a real `agentos-sidecar`.
//!
//! `open_session` requires an agent package projected into `/opt/agentos`. This suite builds a
//! tiny mock ACP package on the fly so it exercises the real sidecar/session path without depending
//! on a locally built Pi adapter.
//!
//! It asserts the durable TypeScript/Rust contract: SQLite-backed listing and history, live event
//! streaming, explicit unload, restoration, and deletion.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use agentos_client::config::{AgentOsConfig, PackageRef};
use agentos_client::{
    AgentOs, OpenSessionInput, PromptInput, ReadHistoryInput, SessionStreamEntry,
};
use agentos_vm_config::VmSqliteDescriptor;
use futures::StreamExt;

const MOCK_AGENT_TYPE: &str = "mock-agent";
const MOCK_SESSION_ID: &str = "mock-session-1";
const MOCK_PROMPT_TEXT: &str = "mock-session-pong";
const MOCK_ACP_ADAPTER: &str = r#"
let buffer = "";
function writeMessage(message) { process.stdout.write(JSON.stringify(message) + "\n"); }
function writeResponse(id, result) { writeMessage({ jsonrpc: "2.0", id, result }); }
process.stdin.resume();
process.stdin.on("data", (chunk) => {
  buffer += chunk instanceof Uint8Array ? new TextDecoder().decode(chunk) : String(chunk);
  while (true) {
    const idx = buffer.indexOf("\n");
    if (idx === -1) break;
    const line = buffer.slice(0, idx);
    buffer = buffer.slice(idx + 1);
    if (!line.trim()) continue;
    const msg = JSON.parse(line);
    if (msg.id === undefined) continue;
    switch (msg.method) {
      case "initialize":
        writeResponse(msg.id, {
          protocolVersion: 1,
          agentInfo: { name: "mock-agent", version: "1.0.0" },
          agentCapabilities: { plan_mode: false, tool_calls: false, promptCapabilities: { image: true, audio: true, embeddedContext: true } },
          modes: { currentModeId: "default", availableModes: [{ id: "default", label: "Default" }] },
          configOptions: [],
        });
        break;
      case "session/new":
        writeResponse(msg.id, {
          sessionId: "__MOCK_SESSION_ID__",
          modes: { currentModeId: "default", availableModes: [{ id: "default", label: "Default" }] },
          configOptions: [],
        });
        break;
      case "session/prompt":
        for (const content of msg.params.prompt) {
          writeMessage({ jsonrpc: "2.0", method: "session/update", params: {
            sessionId: "__MOCK_SESSION_ID__",
            update: { sessionUpdate: "agent_message_chunk", content } } });
        }
        writeResponse(msg.id, { stopReason: "end_turn" });
        break;
      case "session/cancel":
        writeResponse(msg.id, {});
        break;
      default:
        writeMessage({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "Method not found" } });
        break;
    }
  }
});
"#;

fn unique_dir(tag: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("agentos-session-e2e-{tag}-{nonce}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn write_mock_agent_package(root: &Path) -> PathBuf {
    let package = root.join("mock-agent-package");
    std::fs::create_dir_all(package.join("bin")).expect("create package bin");
    std::fs::write(
        package.join("agentos-package.json"),
        r#"{"name":"mock-agent","version":"1.0.0","agent":{"acpEntrypoint":"mock-agent-acp"}}"#,
    )
    .expect("write agentos-package.json");
    let adapter = MOCK_ACP_ADAPTER
        .replace("__MOCK_SESSION_ID__", MOCK_SESSION_ID)
        .replace("__MOCK_PROMPT_TEXT__", MOCK_PROMPT_TEXT);
    let bin = package.join("bin/mock-agent-acp");
    std::fs::write(&bin, format!("#!/usr/bin/env node\n{adapter}\n")).expect("write adapter");
    let mut perms = std::fs::metadata(&bin).expect("stat adapter").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&bin, perms).expect("chmod adapter");
    package
}

#[tokio::test]
async fn durable_session_surface_persists_native_acp_history() {
    if !common::require_sidecar("durable_session_surface_persists_native_acp_history") {
        return;
    }
    let package_root = unique_dir("durable");
    let package_dir = write_mock_agent_package(&package_root);
    common::ensure_sidecar_env();
    let os = AgentOs::create(AgentOsConfig {
        database: Some(VmSqliteDescriptor::SqliteFile {
            path: package_root
                .join("agentos.sqlite")
                .to_string_lossy()
                .into_owned(),
        }),
        packages: vec![PackageRef {
            path: package_dir.to_string_lossy().into_owned(),
        }],
        ..Default::default()
    })
    .await
    .expect("create durable VM");

    os.open_session(OpenSessionInput {
        session_id: None,
        agent: MOCK_AGENT_TYPE.to_owned(),
        cwd: Some(String::from("/home/agentos")),
        additional_directories: None,
        env: None,
        mcp_servers: None,
        permission_policy: None,
        skip_os_instructions: None,
        additional_instructions: None,
    })
    .await
    .expect("open durable session");
    let session = os.get_session(None).await.expect("get durable session");
    assert_eq!(session.session_id, "main");
    assert_eq!(
        os.list_sessions(Default::default())
            .await
            .expect("list")
            .sessions
            .len(),
        1
    );

    let (mut events, _subscription) = os.on_session_event(Some("main"));
    let rich_content = serde_json::json!([
        { "type": "text", "text": MOCK_PROMPT_TEXT },
        {
            "type": "image",
            "data": "iVBORw0KGgo=",
            "mimeType": "image/png",
            "uri": "file:///workspace/pixel.png"
        },
        {
            "type": "resource_link",
            "uri": "https://example.test/reference.txt",
            "name": "reference.txt",
            "mimeType": "text/plain"
        },
        {
            "type": "resource",
            "resource": {
                "uri": "file:///workspace/context.txt",
                "mimeType": "text/plain",
                "text": "context"
            }
        },
        {
            "type": "resource",
            "resource": {
                "uri": "file:///workspace/pixel-copy.png",
                "mimeType": "image/png",
                "blob": "iVBORw0KGgo="
            }
        },
        { "type": "audio", "data": "UklGRg==", "mimeType": "audio/wav" }
    ]);
    let content = serde_json::from_value(rich_content.clone()).expect("rich content blocks");
    let result = os
        .prompt(PromptInput {
            session_id: None,
            idempotency_key: Some(String::from("prompt-1")),
            content,
        })
        .await
        .expect("durable prompt");
    assert_eq!(result.session_id, "main");
    let result_message = serde_json::to_value(&result.message).expect("serialize prompt message");
    assert_eq!(result_message["content"], rich_content);

    let live = tokio::time::timeout(std::time::Duration::from_secs(5), events.next())
        .await
        .expect("live event timeout")
        .expect("live event stream")
        .expect("live event lagged");
    assert!(matches!(live, SessionStreamEntry::Durable(_)));

    let history = os
        .read_history(ReadHistoryInput::default())
        .await
        .expect("history");
    assert_eq!(
        history.events.len(),
        12,
        "six user and six completed agent message blocks"
    );
    assert_eq!(history.events[0].sequence, 1);
    assert_eq!(history.events[11].sequence, 12);
    assert!(serde_json::to_string(&history.events)
        .expect("serialize rich history")
        .contains("iVBORw0KGgo="));

    os.unload_session(None).await.expect("unload");
    assert_eq!(
        os.get_session(None)
            .await
            .expect("SQLite get")
            .latest_sequence,
        12
    );
    os.delete_session(None).await.expect("delete");
    assert!(os.get_session(None).await.is_err());

    os.shutdown().await.expect("shutdown");
    std::fs::remove_dir_all(&package_root).ok();
}
