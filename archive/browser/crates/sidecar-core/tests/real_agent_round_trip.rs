//! End-to-end ACP round-trip: `AcpCore` driving a REAL ACP echo agent (not a mock).
//!
//! A synchronous `AcpHost` (here over `std::process`, spawning `node` + the
//! crate-owned `acp-echo-agent.mjs` fixture) lets `AcpCore::create_session` run the
//! actual `initialize` + `session/new` handshake over real stdin/stdout pipes.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use agentos_protocol::generated::v1::{
    AcpCreateSessionRequest, AcpResponse, AcpRuntimeKind, AcpSessionRequest,
};
use agentos_sidecar_core::host::{AcpHost, AgentOutput, SpawnAgentRequest, SpawnedAgent};
use agentos_sidecar_core::{AcpCore, AcpCoreError};

/// Native `AcpHost` that runs the agent as a `node` child process, reading its
/// stdout on a background thread (std has no non-blocking pipe read).
#[derive(Default)]
struct NodeChildAcpHost {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<Receiver<AgentOutput>>,
    start: Option<Instant>,
}

impl NodeChildAcpHost {
    fn elapsed_ms(&self) -> u64 {
        self.start
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0)
    }
}

impl AcpHost for NodeChildAcpHost {
    fn spawn_agent(&mut self, request: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError> {
        let entrypoint = request
            .entrypoint
            .ok_or_else(|| AcpCoreError::InvalidState("missing agent entrypoint".into()))?;
        if entrypoint != "/opt/agentos/bin/echo" {
            return Err(AcpCoreError::InvalidState(format!(
                "unexpected projected agent entrypoint: {entrypoint}"
            )));
        }
        let mut command = Command::new("node");
        command
            // This native host stands in for the executor's projected command
            // resolver; the guest-visible path above maps to the real fixture.
            .arg(echo_agent_path())
            .args(&request.args)
            .envs(&request.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|error| AcpCoreError::Execution(format!("spawn node agent: {error}")))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(AgentOutput::Stdout(line.into_bytes())).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let pid = child.id();
        self.child = Some(child);
        self.stdin = stdin;
        self.stdout = Some(rx);
        self.start = Some(Instant::now());
        Ok(SpawnedAgent {
            process_id: request.process_id,
            pid: Some(pid),
        })
    }

    fn bind_session(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
        Ok(())
    }

    fn write_stdin(&mut self, _: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| AcpCoreError::Execution("agent stdin closed".into()))?;
        stdin
            .write_all(chunk)
            .and_then(|_| stdin.flush())
            .map_err(|error| AcpCoreError::Execution(format!("write agent stdin: {error}")))
    }

    fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
        self.stdin = None;
        Ok(())
    }

    fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
        if let Some(rx) = &self.stdout {
            if let Ok(output) = rx.try_recv() {
                return Ok(Some(output));
            }
        }
        if let Some(child) = &mut self.child {
            if let Ok(Some(status)) = child.try_wait() {
                return Ok(Some(AgentOutput::Exited(status.code())));
            }
        }
        Ok(None)
    }

    fn kill_agent(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
        Ok(())
    }

    fn wait_for_exit(&mut self, _: &str, timeout_ms: u64) -> Result<Option<i32>, AcpCoreError> {
        let deadline = self.elapsed_ms().saturating_add(timeout_ms);
        while self.elapsed_ms() < deadline {
            if let Some(child) = &mut self.child {
                if let Ok(Some(status)) = child.try_wait() {
                    return Ok(Some(status.code().unwrap_or(0)));
                }
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        Ok(None)
    }

    fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
        Ok(())
    }

    fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
        let manifest = serde_json::json!({
            "name": "echo",
            "agent": {
                "acpEntrypoint": "echo",
            },
        });
        serde_json::to_vec(&manifest)
            .map_err(|error| AcpCoreError::Execution(format!("encode echo manifest: {error}")))
    }

    fn now_ms(&self) -> u64 {
        self.elapsed_ms()
    }
}

fn echo_agent_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-echo-agent.mjs")
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
fn acp_core_runs_a_real_session_round_trip_against_the_echo_agent() {
    if !node_available() {
        eprintln!("skipping: node not available");
        return;
    }
    let agent = echo_agent_path();
    assert!(agent.exists(), "echo agent fixture missing at {agent:?}");

    let mut core = AcpCore::new();
    let mut host = NodeChildAcpHost::default();
    let request = AcpCreateSessionRequest {
        agent_type: "echo".into(),
        runtime: AcpRuntimeKind::JavaScript,
        protocol_version: 1,
        cwd: ".".into(),
        args: Vec::new(),
        env: BTreeMap::new().into_iter().collect(),
        client_capabilities: "{}".into(),
        mcp_servers: "[]".into(),
        additional_directories: Vec::new(),
        additional_instructions: None,
        skip_os_instructions: false,
    };

    // Real round-trip: spawn node, run initialize + session/new over real pipes.
    let response = core
        .create_session(&mut host, "conn-real", &request)
        .expect("real ACP create_session round-trip");
    let session_id = match response {
        AcpResponse::AcpSessionCreatedResponse(created) => {
            assert_eq!(created.session_id, "echo-session-1");
            created.session_id
        }
        other => panic!("expected created response, got {other:?}"),
    };
    assert_eq!(core.session_count(), 1);

    // Continue the real round-trip: drive a session/prompt through the SAME live
    // node child and assert the agent's turn comes back (create -> prompt e2e).
    let prompt = AcpSessionRequest {
        session_id: session_id.clone(),
        method: "session/prompt".into(),
        params: Some(r#"{"prompt":[{"type":"text","text":"hello"}]}"#.into()),
    };
    let prompt_response = core
        .session_request(&mut host, "conn-real", &prompt)
        .expect("real ACP session/prompt round-trip");
    match prompt_response {
        AcpResponse::AcpSessionRpcResponse(rpc) => {
            assert_eq!(rpc.session_id, session_id);
            let body: serde_json::Value = serde_json::from_str(&rpc.response).unwrap();
            assert_eq!(body["result"]["stopReason"], serde_json::json!("end_turn"));
        }
        other => panic!("expected rpc response, got {other:?}"),
    }

    // Ownership is enforced on the live session: a different connection cannot drive
    // it and gets the same unknown-session error.
    let err = core
        .session_request(&mut host, "conn-other", &prompt)
        .expect_err("non-owner prompt must fail closed");
    assert_eq!(err.code(), "invalid_state");
}
