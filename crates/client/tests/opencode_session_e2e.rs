//! Packed OpenCode ACP smoke test against the real AgentOS sidecar.
//!
//! It proves that the production `.aospkg` projects, its native upstream ACP
//! adapter initializes, exposes its real model directory, creates a session,
//! and completes a prompt through a host llmock LLM inside an AgentOS VM.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use agentos_client::config::{
    AgentOsConfig, AgentOsLimits, JsRuntimeLimits, PackageRef, PatternPermissions, PermissionMode,
    Permissions,
};
use agentos_client::fs::MkdirOptions;
use agentos_client::{
    AgentOs, ContentBlock, ExecOptions, ListSessionsInput, OpenSessionInput, PromptInput,
};

const LLMOCK_SENTINEL: &str = "PONG_FROM_LLMOCK";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn opencode_package_path() -> Option<PathBuf> {
    let package = repo_root().join("registry/agent/opencode/dist/package.aospkg");
    package.is_file().then_some(package)
}

fn coreutils_package_path() -> PathBuf {
    let package = repo_root().join("registry/software/coreutils/dist/package.aospkg");
    assert!(
        package.is_file(),
        "Coreutils package is not built; run `pnpm --dir registry/software/coreutils build`"
    );
    package
}

struct LlmockServer {
    child: Child,
    url: String,
}

impl LlmockServer {
    #[allow(clippy::zombie_processes)]
    fn start() -> Self {
        let root = repo_root();
        let mut child = Command::new("node")
            .arg(root.join("crates/client/tests/helpers/llmock-server.mjs"))
            .current_dir(&root)
            .env("LLMOCK_SENTINEL", LLMOCK_SENTINEL)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn host llmock server");
        let mut stdout = BufReader::new(child.stdout.take().expect("llmock stdout"));
        let mut line = String::new();
        loop {
            line.clear();
            let read = stdout.read_line(&mut line).expect("read llmock stdout");
            assert_ne!(read, 0, "llmock exited before printing its URL");
            if let Some(url) = line.trim().strip_prefix("LLMOCK_URL=") {
                return Self {
                    child,
                    url: url.to_string(),
                };
            }
        }
    }

    fn port(&self) -> u16 {
        self.url
            .rsplit(':')
            .next()
            .and_then(|tail| tail.trim_end_matches('/').parse().ok())
            .expect("parse llmock port")
    }
}

impl Drop for LlmockServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn packed_opencode_initializes_and_creates_session() {
    if !common::require_sidecar("packed_opencode_initializes_and_creates_session") {
        return;
    }
    let package_path = opencode_package_path()
        .expect("OpenCode package is not built; run `pnpm --dir registry/agent/opencode build`");
    let coreutils_path = coreutils_package_path();
    let llmock = LlmockServer::start();

    let os = AgentOs::create(AgentOsConfig {
        loopback_exempt_ports: vec![llmock.port()],
        packages: vec![
            PackageRef {
                path: package_path.to_string_lossy().into_owned(),
            },
            PackageRef {
                path: coreutils_path.to_string_lossy().into_owned(),
            },
        ],
        limits: Some(AgentOsLimits {
            js_runtime: Some(JsRuntimeLimits {
                // Upstream OpenCode's generated provider catalog and server
                // currently peak above 256 MiB during the first directory snapshot.
                v8_heap_limit_mb: Some(512),
                ..Default::default()
            }),
            ..Default::default()
        }),
        permissions: Some(Permissions {
            network: Some(PatternPermissions::Mode(PermissionMode::Allow)),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .expect("create VM with packed OpenCode package");

    os.mkdir("/home/agentos", MkdirOptions { recursive: true })
        .await
        .expect("create OpenCode home");
    os.mkdir("/workspace", MkdirOptions { recursive: true })
        .await
        .expect("create OpenCode workspace");
    os.mkdir(
        "/home/agentos/.config/opencode",
        MkdirOptions { recursive: true },
    )
    .await
    .expect("create OpenCode config directory");
    let config = serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "autoupdate": false,
        "share": "disabled",
        "snapshot": false,
        "model": "anthropic/claude-sonnet-4-6",
        "provider": {
            "anthropic": {
                "options": {
                    "baseURL": format!("{}/v1", llmock.url.trim_end_matches('/'))
                }
            }
        }
    })
    .to_string();
    os.write_file(
        "/home/agentos/.config/opencode/opencode.json",
        config.as_str(),
    )
    .await
    .expect("write OpenCode llmock config");

    let temp_dir_probe = os
        .exec_argv(
            "node",
            &[
                "-e".to_string(),
                r#"(async () => {
const { mkdtemp } = require("node:fs");
const fsNamespace = await import("node:fs");
const result = await new Promise((resolve) => mkdtemp("/tmp/opencode-", (error, path) => resolve({
  error: error ? { message: error.message, code: error.code } : null,
  path,
})));
console.log(JSON.stringify({
  ...result,
  cjsOpen: typeof require("node:fs").open,
  esmOpen: typeof fsNamespace.open,
  defaultOpen: typeof fsNamespace.default?.open,
}));
})();"#
                    .to_string(),
            ],
            ExecOptions::default(),
        )
        .await
        .expect("run Node fs.mkdtemp compatibility probe");
    assert_eq!(
        temp_dir_probe.exit_code, 0,
        "node:fs mkdtemp failed: stdout={:?} stderr={:?}",
        temp_dir_probe.stdout, temp_dir_probe.stderr
    );
    assert!(
        temp_dir_probe.stdout.contains(r#""error":null"#),
        "node:fs mkdtemp callback returned an error: {:?}",
        temp_dir_probe.stdout
    );
    assert!(
        temp_dir_probe.stdout.contains(r#""esmOpen":"function""#),
        "node:fs ESM namespace omitted open(): {:?}",
        temp_dir_probe.stdout
    );

    let agent = os
        .projected_agents()
        .into_iter()
        .find(|agent| agent.id == "opencode")
        .expect("packed OpenCode agent should be projected");
    assert_eq!(
        agent.adapter_entrypoint,
        "/opt/agentos/bin/agentos-opencode-acp"
    );

    let env = BTreeMap::from([
        ("HOME".to_string(), "/home/agentos".to_string()),
        ("OPENCODE_DISABLE_AUTOUPDATE".to_string(), "1".to_string()),
        ("OPENCODE_DISABLE_FILEWATCHER".to_string(), "1".to_string()),
        ("OPENCODE_DISABLE_LSP_DOWNLOAD".to_string(), "1".to_string()),
        ("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string()),
        ("OPENCODE_LOG_LEVEL".to_string(), "DEBUG".to_string()),
        ("ANTHROPIC_API_KEY".to_string(), "mock-key".to_string()),
    ]);
    let session_id = "opencode-e2e";
    let session_result = tokio::time::timeout(
        Duration::from_secs(60),
        os.open_session(OpenSessionInput {
            session_id: Some(session_id.to_string()),
            agent: "opencode".to_string(),
            cwd: Some("/workspace".to_string()),
            additional_directories: None,
            env: Some(env),
            mcp_servers: None,
            permission_policy: None,
            skip_os_instructions: Some(true),
            additional_instructions: None,
        }),
    )
    .await
    .expect("OpenCode ACP initialize/session creation timed out");
    if let Err(error) = &session_result {
        let log = os
            .exec_argv(
                "node",
                &[
                    "-e".to_string(),
                    r#"const fs = require("node:fs");
const path = "/home/agentos/.local/share/opencode/log/opencode.log";
try {
  process.stdout.write(fs.readFileSync(path, "utf8"));
}
catch (error) { process.stderr.write(String(error)); process.exitCode = 1; }"#
                        .to_string(),
                ],
                ExecOptions::default(),
            )
            .await
            .expect("read OpenCode diagnostic log");
        panic!(
            "OpenCode ACP initialize/session/new must succeed: {error}; log stdout={:?} stderr={:?}",
            log.stdout, log.stderr
        );
    }
    session_result.expect("checked above");
    let session = os
        .get_session(Some(session_id))
        .await
        .expect("read opened OpenCode session");

    assert!(!session.session_id.is_empty());
    assert!(os
        .list_sessions(ListSessionsInput::default())
        .await
        .expect("list durable sessions")
        .sessions
        .iter()
        .any(|entry| entry.session_id == session.session_id));
    let model_options = os
        .get_session_config(Some(&session.session_id))
        .await
        .expect("read OpenCode session config")
        .options
        .into_iter()
        .map(|option| serde_json::to_value(option).expect("serialize config option"))
        .find(|option| {
            option.get("category").and_then(|value| value.as_str()) == Some("model")
                || option.get("id").and_then(|value| value.as_str()) == Some("model")
        })
        .expect("native OpenCode ACP should expose a model selector");
    let model_count = model_options
        .get("options")
        .and_then(|value| value.as_array())
        .map_or(0, Vec::len);
    assert!(
        model_count > 1,
        "native OpenCode ACP should expose more than one model; got {model_options:?}"
    );
    assert_eq!(
        model_options
            .get("currentValue")
            .and_then(|value| value.as_str()),
        Some("anthropic/claude-sonnet-4-6"),
        "native OpenCode ACP should honor the configured current model"
    );

    let prompt = tokio::time::timeout(
        Duration::from_secs(60),
        os.prompt(PromptInput {
            session_id: Some(session.session_id.clone()),
            idempotency_key: Some("opencode-e2e-prompt".to_string()),
            content: vec![serde_json::from_value::<ContentBlock>(serde_json::json!({
                "type": "text",
                "text": "Reply with the sentinel."
            }))
            .expect("construct text content")],
        }),
    )
    .await
    .expect("OpenCode prompt timed out")
    .expect("OpenCode prompt should succeed against llmock");
    let prompt_text = serde_json::to_string(&prompt.message).expect("serialize prompt message");
    if !prompt_text.contains(LLMOCK_SENTINEL) {
        let log = os
            .exec_argv(
                "node",
                &[
                    "-e".to_string(),
                    r#"const fs = require("node:fs");
const path = "/home/agentos/.local/share/opencode/log/opencode.log";
process.stdout.write(fs.readFileSync(path, "utf8"));"#
                        .to_string(),
                ],
                ExecOptions::default(),
            )
            .await
            .expect("read OpenCode empty prompt log");
        panic!(
            "OpenCode prompt should include llmock sentinel; got {prompt:?}; log stdout={:?} stderr={:?}",
            log.stdout, log.stderr
        );
    }

    os.delete_session(Some(&session.session_id))
        .await
        .expect("delete OpenCode session");
    os.shutdown().await.expect("shutdown OpenCode VM");
}
