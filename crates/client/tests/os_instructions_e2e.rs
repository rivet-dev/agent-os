//! End-to-end coverage for sidecar-owned system-prompt injection at `open_session`.
//!
//! The base prompt is no longer baked into a guest file (`/etc/agentos/instructions.md` is gone);
//! the Agent OS client passes create-time additions and generated tool docs to the wrapper sidecar,
//! which assembles them with the base prompt and passes the result through the adapter-neutral
//! launch environment. This test resolves a tiny mock ACP adapter through the real module-access
//! path, launches a `pi` session, and asserts the adapter actually observed the injected prompt.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use agentos_client::config::{
    node_modules_mount, AgentOsConfig, AgentOsSidecarConfig, Binding, Bindings, FsPermissions,
    PackageRef, PatternPermissions, PermissionMode, Permissions,
};
use agentos_client::{AgentOs, OpenSessionInput};
use agentos_vm_config::VmSqliteDescriptor;
use serde_json::json;
use uuid::Uuid;

/// A mock ACP adapter that answers `initialize` / `session/new` and echoes the generic system-prompt
/// environment value in `agentInfo` so the test can read it without guest filesystem timing.
const MOCK_ACP_ADAPTER: &str = r#"
let buffer = "";
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
    let result;
    switch (msg.method) {
      case "initialize":
        result = {
          protocolVersion: 1,
          agentInfo: { name: process.env.ACP_APPEND_SYSTEM_PROMPT || "", version: "1.0.0" },
        };
        break;
      case "session/new":
        result = { sessionId: "__MOCK_SESSION_ID__" };
        break;
      default:
        process.stdout.write(
          JSON.stringify({ jsonrpc: "2.0", id: msg.id, error: { code: -32601, message: "Method not found" } }) + "\n",
        );
        continue;
    }
    process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }) + "\n");
  }
});

setInterval(() => {}, 1000);
"#;

const ADDITIONAL_MARKER: &str = "rust-client-extra-instructions";

/// Allow-all permissions so the mock adapter can spawn, read its module-access bin, and write the
/// prompt probe.
fn allow_all_permissions() -> Permissions {
    Permissions {
        fs: Some(FsPermissions::Mode(PermissionMode::Allow)),
        network: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        child_process: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        process: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        env: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        binding: Some(PatternPermissions::Mode(PermissionMode::Allow)),
    }
}

/// Lay out a fake `node_modules/@agentos-software/pi` that is also a projectable
/// agentOS package, so the sidecar resolves `pi` from `/opt/agentos`.
fn write_mock_pi_adapter(module_root: &std::path::Path) -> std::path::PathBuf {
    let package_dir = module_root
        .join("node_modules")
        .join("@agentos-software")
        .join("pi");
    std::fs::create_dir_all(&package_dir).expect("create mock adapter package dir");
    std::fs::write(
        package_dir.join("package.json"),
        r#"{ "name": "@agentos-software/pi", "version": "0.0.0", "bin": "./adapter.mjs" }"#,
    )
    .expect("write mock adapter package.json");
    std::fs::write(
        package_dir.join("agentos-package.json"),
        r#"{"name":"pi","version":"0.0.0","agent":{"acpEntrypoint":"pi"}}"#,
    )
    .expect("write mock agentos-package.json");
    let adapter = MOCK_ACP_ADAPTER.replace(
        "__MOCK_SESSION_ID__",
        &format!("mock-session-{}", Uuid::new_v4()),
    );
    let adapter_path = package_dir.join("adapter.mjs");
    std::fs::write(&adapter_path, format!("#!/usr/bin/env node\n{adapter}"))
        .expect("write mock adapter entrypoint");
    std::fs::set_permissions(&adapter_path, std::fs::Permissions::from_mode(0o755))
        .expect("make mock adapter executable");
    package_dir
}

async fn launch_pi_session_and_read_prompt(options: OpenSessionInput) -> String {
    launch_pi_session_with_tools_and_read_prompt(options, Vec::new()).await
}

async fn launch_pi_session_with_tools_and_read_prompt(
    options: OpenSessionInput,
    bindings: Vec<Bindings>,
) -> String {
    let module_access_dir =
        std::env::temp_dir().join(format!("agentos-client-os-instructions-{}", Uuid::new_v4()));
    let package_dir = write_mock_pi_adapter(&module_access_dir);

    let prompt = run_session(&module_access_dir, &package_dir, options, bindings).await;

    std::fs::remove_dir_all(&module_access_dir).ok();
    prompt
}

async fn run_session(
    module_access_dir: &Path,
    package_dir: &Path,
    options: OpenSessionInput,
    bindings: Vec<Bindings>,
) -> String {
    let os = AgentOs::create(AgentOsConfig {
        database: Some(VmSqliteDescriptor::SqliteFile {
            path: module_access_dir
                .join("agentos.sqlite")
                .to_string_lossy()
                .into_owned(),
        }),
        mounts: vec![node_modules_mount(
            module_access_dir
                .join("node_modules")
                .to_string_lossy()
                .into_owned(),
        )],
        packages: vec![PackageRef {
            path: package_dir.to_string_lossy().into_owned(),
        }],
        sidecar: Some(AgentOsSidecarConfig::Shared {
            pool: Some(format!("os-instructions-{}", Uuid::new_v4())),
        }),
        bindings,
        permissions: Some(allow_all_permissions()),
        ..Default::default()
    })
    .await
    .expect("create VM with module access for mock adapter");

    os.open_session(options)
        .await
        .expect("open pi session against mock adapter");

    let agent_info = os
        .get_session_agent_info(Some("main"))
        .await
        .expect("read agent info")
        .expect("mock adapter should report agent info");
    let prompt = agent_info.name;

    os.shutdown().await.expect("shutdown VM");
    prompt
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_session_injects_assembled_system_prompt() {
    if !common::sidecar_available() {
        panic!(
            "create_session_injects_assembled_system_prompt: sidecar binary is not built; build it with `cargo build -p agentos-sidecar`"
        );
    }
    common::ensure_sidecar_env();

    let prompt = launch_pi_session_and_read_prompt(OpenSessionInput {
        session_id: None,
        agent: String::from("pi"),
        cwd: None,
        additional_directories: None,
        env: None,
        mcp_servers: None,
        permission_policy: None,
        skip_os_instructions: None,
        additional_instructions: Some(ADDITIONAL_MARKER.to_string()),
    })
    .await;

    assert!(
        prompt.contains("# agentOS"),
        "base OS instructions are injected: {prompt:?}"
    );
    assert!(
        prompt.contains(ADDITIONAL_MARKER),
        "create-time additional instructions are appended: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_session_injects_binding_reference_from_client_config() {
    if !common::sidecar_available() {
        panic!(
            "create_session_injects_binding_reference_from_client_config: sidecar binary is not built; build it with `cargo build -p agentos-sidecar`"
        );
    }
    common::ensure_sidecar_env();

    let prompt = launch_pi_session_with_tools_and_read_prompt(
        OpenSessionInput {
            session_id: None,
            agent: String::from("pi"),
            cwd: None,
            additional_directories: None,
            env: None,
            mcp_servers: None,
            permission_policy: None,
            skip_os_instructions: None,
            additional_instructions: None,
        },
        vec![Bindings {
            name: "weather".to_string(),
            description: "Weather lookup tools.".to_string(),
            bindings: vec![Binding {
                name: "forecast".to_string(),
                description: "Get a forecast.".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "zipCode": { "type": "string" },
                    },
                    "required": ["zipCode"],
                }),
                timeout_ms: None,
                execute: Arc::new(|_input| Box::pin(async { Ok(json!({ "ok": true })) })),
            }],
        }],
    )
    .await;

    assert!(
        prompt.contains("## Available Host Bindings"),
        "client-generated tool reference is injected: {prompt:?}"
    );
    assert!(
        prompt.contains("`agentos-weather forecast --zip-code <string>`"),
        "tool reference includes CLI command and schema-derived flags: {prompt:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_session_skip_os_instructions_drops_base_but_keeps_additional() {
    if !common::sidecar_available() {
        panic!(
            "create_session_skip_os_instructions_drops_base_but_keeps_additional: sidecar binary is not built; build it with `cargo build -p agentos-sidecar`"
        );
    }
    common::ensure_sidecar_env();

    let prompt = launch_pi_session_and_read_prompt(OpenSessionInput {
        session_id: None,
        agent: String::from("pi"),
        cwd: None,
        additional_directories: None,
        env: Some(BTreeMap::new()),
        mcp_servers: None,
        permission_policy: None,
        skip_os_instructions: Some(true),
        additional_instructions: Some(ADDITIONAL_MARKER.to_string()),
    })
    .await;

    assert!(
        !prompt.contains("# agentOS"),
        "skip_os_instructions drops the base prompt: {prompt:?}"
    );
    assert!(
        prompt.contains(ADDITIONAL_MARKER),
        "skip_os_instructions still injects additional instructions: {prompt:?}"
    );
}
