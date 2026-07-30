//! The host-free ACP engine: owns session state and dispatches ACP requests,
//! driving host-coupled work through the synchronous [`AcpHost`] seam.
//!
//! Ported from `agentos-sidecar::acp_extension` (async) to synchronous, host-free
//! form. All five ACP requests are ported onto the [`AcpHost`] seam: the
//! pure-state ones (`get_session_state`, `close_session`), the bootstrap one
//! (`create_session`), the in-session RPC one (`session_request`, i.e.
//! `session/prompt` + other in-session methods), and `resume_session` (native
//! `session/load` tier with the universal `session/new` fallback). Notification
//! streaming as ACP events and the `session/cancel` not-found fallback remain a
//! documented follow-up that layers on the same loop (see `session_request`).

use std::collections::BTreeMap;

use agentos_protocol::generated::v1::{
    AcpCloseSessionRequest, AcpCreateSessionRequest, AcpDeliverAgentOutputRequest,
    AcpGetSessionStateRequest, AcpPendingResponse, AcpRequest, AcpResponse,
    AcpResumeSessionRequest, AcpRuntimeKind, AcpSessionClosedResponse, AcpSessionRequest,
    AcpSessionResumedResponse, AcpSessionRpcResponse,
};
use serde_json::{json, Map, Value};

use crate::host::{AcpHost, SpawnAgentRequest};
use crate::json_rpc::send_json_rpc;
use crate::session::AcpSessionRecord;
use crate::AcpCoreError;

/// Matches the native sidecar's `SESSION_CLOSE_TIMEOUT` (5s).
const SESSION_CLOSE_TIMEOUT_MS: u64 = 5_000;
/// Matches the native bootstrap/control-operation timeout policy.
const INITIALIZE_TIMEOUT_MS: u64 = 60_000;
const SESSION_NEW_TIMEOUT_MS: u64 = 120_000;
const MAX_ACP_ADDITIONAL_DIRECTORIES: usize = 128;
const MAX_ACP_GUEST_PATH_BYTES: usize = 4_096;

/// Matches the native `ACP_RESUME_PROTOCOL_VERSION`.
const ACP_RESUME_PROTOCOL_VERSION: i32 = 1;
/// Matches the native `DEFAULT_RESUME_CLIENT_CAPABILITIES`.
const DEFAULT_RESUME_CLIENT_CAPABILITIES: &str =
    "{\"fs\":{\"readTextFile\":true,\"writeTextFile\":true},\"terminal\":true,\"session\":{\"configOptions\":{\"boolean\":{}}}}";
/// Transcript-continuation preamble armed by the resume fallback tier; matches the
/// native `CONTINUATION_PREAMBLE`.
const CONTINUATION_PREAMBLE: &str = "You are continuing an earlier session. The full prior transcript is at `{path}`. Read it with your file tools if you need context before answering.";

/// Agent launch parameters resolved from a projected `/opt/agentos` package
/// manifest. The npm-agnostic client sends only the agent name; the sidecar owns
/// the name -> package -> entrypoint/env/launchArgs resolution. Mirrors the native
/// sidecar's `ResolvedAgent`.
struct ResolvedAgent {
    entrypoint: String,
    env: BTreeMap<String, String>,
    launch_args: Vec<String>,
}

/// The subset of `agentos-package.json` the sidecar needs to launch an agent.
#[derive(serde::Deserialize)]
struct AgentPackageManifest {
    #[serde(default)]
    agent: Option<AgentPackageAgentBlock>,
}

#[derive(serde::Deserialize)]
struct AgentPackageAgentBlock {
    #[serde(rename = "acpEntrypoint", default)]
    acp_entrypoint: String,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(rename = "launchArgs", default)]
    launch_args: Vec<String>,
}

/// Resolve an agent name to its launch parameters by reading the projected
/// manifest at `/opt/agentos/pkgs/<name>/current/agentos-package.json` over the host
/// filesystem seam. A missing file, a missing `agent` block, or an empty
/// `agent.acpEntrypoint` all map to a single typed "unknown agent" error. Mirrors
/// the native sidecar `resolve_agent`.
fn resolve_agent<H: AcpHost>(
    host: &mut H,
    agent_type: &str,
) -> Result<ResolvedAgent, AcpCoreError> {
    let unknown = || {
        AcpCoreError::InvalidState(format!(
            "unknown agent type \"{agent_type}\": no projected /opt/agentos/pkgs/{agent_type} package \
             with an agent.acpEntrypoint — pass its package to AgentOs software"
        ))
    };
    let path = format!("/opt/agentos/pkgs/{agent_type}/current/agentos-package.json");
    let bytes = host.read_file(&path).map_err(|_| unknown())?;
    let manifest: AgentPackageManifest = serde_json::from_slice(&bytes).map_err(|_| unknown())?;
    let agent = manifest.agent.ok_or_else(&unknown)?;
    if agent.acp_entrypoint.is_empty() {
        return Err(unknown());
    }
    Ok(ResolvedAgent {
        entrypoint: format!("/opt/agentos/bin/{}", agent.acp_entrypoint),
        env: agent.env,
        launch_args: agent.launch_args,
    })
}

/// Host-free ACP session engine. The native and browser sidecars each hold one of
/// these and feed it decoded requests plus the caller's connection id (for the
/// per-connection ownership checks) and an [`AcpHost`] for the host-coupled steps.
#[derive(Debug, Default)]
pub struct AcpCore {
    sessions: BTreeMap<String, AcpSessionRecord>,
    next_process_id: u64,
    /// In-flight RESUMABLE create_session handshakes (browser path). The synchronous
    /// `create_session` blocks; the resumable path (begin_create_session +
    /// feed_agent_output) never does, so the single-threaded kernel worker can
    /// release the wasm borrow between steps and service the agent's own syscalls on
    /// fresh, non-nested calls (see AGENTOS-WEB-ASYNC-AGENTS.md §3.2 + the
    /// pushFrame-re-entrancy constraint). Native keeps using the blocking path.
    pending_creates: BTreeMap<String, PendingCreate>,
    /// In-flight RESUMABLE session/prompt (and other in-session RPC) requests,
    /// keyed by the agent's process id.
    pending_prompts: BTreeMap<String, PendingPrompt>,
}

/// State of one in-flight resumable `session/prompt` (or other in-session RPC).
#[derive(Debug)]
struct PendingPrompt {
    session_id: String,
    rpc_id: i64,
    stdout_buffer: String,
}

/// State of one in-flight resumable `create_session` handshake.
#[derive(Debug)]
struct PendingCreate {
    owner_connection_id: String,
    agent_type: String,
    pid: Option<u32>,
    protocol_version: i32,
    cwd: String,
    mcp_servers: Value,
    additional_directories: Vec<String>,
    step: CreateStep,
    stdout_buffer: String,
    init_result: Option<Map<String, Value>>,
}

#[derive(Debug, PartialEq, Eq)]
enum CreateStep {
    AwaitingInitialize,
    AwaitingSessionNew,
}

/// Outcome of feeding agent output into a resumable handshake.
#[derive(Debug)]
pub enum ResumeStep {
    /// More agent output is needed; the interaction is still in flight.
    Pending,
    /// The interaction completed; deliver this response as the (deferred) result.
    Done(AcpResponse),
}

/// Result of the ACP bootstrap handshake (initialize + session/new).
struct SessionBootstrap {
    session_id: String,
    modes: Option<String>,
    config_options: Vec<String>,
    agent_capabilities: Option<String>,
    agent_info: Option<String>,
    stdout_buffer: String,
}

impl AcpCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    fn allocate_process_id(&mut self, prefix: &str) -> String {
        let id = self.next_process_id;
        self.next_process_id += 1;
        format!("{prefix}-{id}")
    }

    /// Insert/replace a session record (used by the create/resume handlers once a
    /// process is live).
    pub fn insert_session(&mut self, record: AcpSessionRecord) {
        self.sessions.insert(record.session_id.clone(), record);
    }

    /// `session/state`: pure state lookup with per-connection ownership. A non-owner
    /// (or missing session) fails closed with the SAME error so another connection's
    /// session is not revealed across the tenant boundary.
    pub fn get_session_state(
        &self,
        caller_connection_id: &str,
        request: &AcpGetSessionStateRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        let unknown =
            || AcpCoreError::InvalidState(format!("unknown ACP session {}", request.session_id));
        let session = self.sessions.get(&request.session_id).ok_or_else(unknown)?;
        if session.owner_connection_id != caller_connection_id {
            return Err(unknown());
        }
        Ok(AcpResponse::AcpSessionStateResponse(
            session.state_response(),
        ))
    }

    /// `session/close`: owner-only teardown. Removes the record, then SIGTERM →
    /// (timeout) → SIGKILL the agent process through the host seam. Mirrors the
    /// native `close_session` flow.
    pub fn close_session<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpCloseSessionRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        let owned_by_caller = self
            .sessions
            .get(&request.session_id)
            .is_some_and(|session| session.owner_connection_id == caller_connection_id);
        let session = if owned_by_caller {
            self.sessions.remove(&request.session_id)
        } else {
            None
        };
        let Some(session) = session else {
            return Err(AcpCoreError::InvalidState(format!(
                "unknown ACP session {}",
                request.session_id
            )));
        };

        let mut acp_close_error = None;
        match serialized_session_capability(session.agent_capabilities.as_deref(), "close") {
            Ok(true) if !session.closed => {
                let request_id = session.next_request_id;
                let request = json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "session/close",
                    "params": { "sessionId": session.session_id },
                });
                let mut stdout = session.stdout_buffer.clone();
                match send_json_rpc(
                    host,
                    &session.process_id,
                    &request,
                    request_id,
                    Some(SESSION_CLOSE_TIMEOUT_MS),
                    &mut stdout,
                ) {
                    Ok(response) => {
                        if let Err(error) = response_result(response, "ACP session/close") {
                            acp_close_error = Some(error);
                        }
                    }
                    Err(error) => acp_close_error = Some(error),
                }
            }
            Ok(_) => {}
            Err(error) => acp_close_error = Some(error),
        }

        let _ = host.close_stdin(&session.process_id);
        // Mirror of the native `close_session` short-circuit: an adapter that
        // already exited (crash / OOM / idle eviction, recorded on the session
        // as `closed`) has no future exit to wait for, and signalling its
        // reaped PID fails with a process-gone error. Skip the SIGTERM → wait
        // → SIGKILL → wait dance (~2× `SESSION_CLOSE_TIMEOUT_MS` of dead
        // waiting) in that case.
        let adapter_already_gone = session.closed || {
            let sigterm = host.kill_agent(&session.process_id, "SIGTERM");
            matches!(&sigterm, Err(error) if is_process_already_gone_error(error))
        };
        if !adapter_already_gone
            && host
                .wait_for_exit(&session.process_id, SESSION_CLOSE_TIMEOUT_MS)?
                .is_none()
        {
            let sigkill = host.kill_agent(&session.process_id, "SIGKILL");
            if !matches!(&sigkill, Err(error) if is_process_already_gone_error(error)) {
                let _ = host.wait_for_exit(&session.process_id, SESSION_CLOSE_TIMEOUT_MS)?;
            }
        }

        if let Some(error) = acp_close_error {
            return Err(error);
        }
        Ok(AcpResponse::AcpSessionClosedResponse(
            AcpSessionClosedResponse {
                session_id: request.session_id.clone(),
            },
        ))
    }

    /// `session/create`: launch the agent adapter, run the ACP bootstrap handshake
    /// (`initialize` then `session/new`) over the sync seam, and record the session.
    /// NOTE: opencode-specific prompt injection / config-option derivation from the
    /// native `create_session` are deferred follow-ups; the minimal core launches the
    /// adapter as configured and records what the handshake returns.
    pub fn create_session<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpCreateSessionRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        let resolved = resolve_agent(host, &request.agent_type)?;
        let process_id = self.allocate_process_id("acp-agent");
        let mut env: BTreeMap<String, String> = request.env.clone().into_iter().collect();
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        env.insert(
            String::from("AGENTOS_EAGER_STDIN_HANDLE"),
            String::from("1"),
        );
        // Manifest env applies as DEFAULTS; caller/base env wins on conflicts.
        for (key, value) in &resolved.env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // Manifest launch args first, then any caller-supplied args.
        let mut args = resolved.launch_args.clone();
        args.extend(request.args.iter().cloned());

        let spawned = host.spawn_agent(SpawnAgentRequest {
            process_id: process_id.clone(),
            runtime: request.runtime.clone(),
            entrypoint: Some(resolved.entrypoint.clone()),
            command: None,
            args,
            env,
            cwd: Some(request.cwd.clone()),
        })?;

        let bootstrap = match self.bootstrap_session(host, request, &process_id) {
            Ok(bootstrap) => bootstrap,
            Err(error) => {
                let _ = host.kill_agent(&process_id, "SIGKILL");
                return Err(error);
            }
        };

        let session = AcpSessionRecord {
            session_id: bootstrap.session_id.clone(),
            owner_connection_id: caller_connection_id.to_string(),
            agent_type: request.agent_type.clone(),
            process_id: process_id.clone(),
            pid: spawned.pid,
            modes: bootstrap.modes,
            config_options: bootstrap.config_options,
            agent_capabilities: bootstrap.agent_capabilities,
            agent_info: bootstrap.agent_info,
            stdout_buffer: bootstrap.stdout_buffer,
            next_request_id: 3,
            closed: false,
            exit_code: None,
            pending_preamble: None,
        };

        host.bind_session(&session.session_id, &process_id)?;
        let response = AcpResponse::AcpSessionCreatedResponse(session.created_response());
        self.sessions.insert(session.session_id.clone(), session);
        Ok(response)
    }

    /// RESUMABLE `create_session` — start (browser path). Spawns the agent and writes
    /// the `initialize` request, then RETURNS without waiting (no `poll_output`). The
    /// caller feeds the agent's stdout back via [`feed_agent_output`]; between calls
    /// the kernel worker is free (the wasm borrow is released), so it can service the
    /// agent's own syscalls on fresh, non-nested calls. Returns the process id used
    /// as the handshake handle.
    pub fn begin_create_session<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpCreateSessionRequest,
    ) -> Result<String, AcpCoreError> {
        let resolved = resolve_agent(host, &request.agent_type)?;
        let process_id = self.allocate_process_id("acp-agent");
        let mut env: BTreeMap<String, String> = request.env.clone().into_iter().collect();
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        env.insert(
            String::from("AGENTOS_EAGER_STDIN_HANDLE"),
            String::from("1"),
        );
        // Manifest env applies as DEFAULTS; caller/base env wins on conflicts.
        for (key, value) in &resolved.env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }
        // Manifest launch args first, then any caller-supplied args.
        let mut args = resolved.launch_args.clone();
        args.extend(request.args.iter().cloned());

        let spawned = host.spawn_agent(SpawnAgentRequest {
            process_id: process_id.clone(),
            runtime: request.runtime.clone(),
            entrypoint: Some(resolved.entrypoint.clone()),
            command: None,
            args,
            env,
            cwd: Some(request.cwd.clone()),
        })?;

        let client_capabilities =
            parse_json_text(&request.client_capabilities, "clientCapabilities")?;
        let mcp_servers = parse_json_text(&request.mcp_servers, "mcpServers")?;
        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": request.protocol_version,
                "clientCapabilities": client_capabilities,
            },
        });
        if let Err(error) = write_json_line(host, &process_id, &initialize) {
            let _ = host.kill_agent(&process_id, "SIGKILL");
            return Err(error);
        }

        self.pending_creates.insert(
            process_id.clone(),
            PendingCreate {
                owner_connection_id: caller_connection_id.to_string(),
                agent_type: request.agent_type.clone(),
                pid: spawned.pid,
                protocol_version: request.protocol_version,
                cwd: request.cwd.clone(),
                mcp_servers,
                additional_directories: request.additional_directories.clone(),
                step: CreateStep::AwaitingInitialize,
                stdout_buffer: String::new(),
                init_result: None,
            },
        );
        Ok(process_id)
    }

    /// RESUMABLE — feed agent stdout into whatever interaction is in flight for this
    /// process (a `create_session` handshake or a `session/prompt`), advancing it
    /// without ever blocking. Returns [`ResumeStep::Done`] with the response when the
    /// interaction completes, else [`ResumeStep::Pending`]. The kernel worker calls
    /// this across separate `pushFrame`s and services the agent's syscalls in between
    /// (legal — not nested).
    pub fn feed_agent_output<H: AcpHost>(
        &mut self,
        host: &mut H,
        process_id: &str,
        chunk: &[u8],
    ) -> Result<ResumeStep, AcpCoreError> {
        if self.pending_creates.contains_key(process_id) {
            self.feed_create(host, process_id, chunk)
        } else if self.pending_prompts.contains_key(process_id) {
            self.feed_prompt(process_id, chunk)
        } else {
            Err(AcpCoreError::InvalidState(format!(
                "no pending ACP interaction for {process_id}"
            )))
        }
    }

    fn feed_create<H: AcpHost>(
        &mut self,
        host: &mut H,
        process_id: &str,
        chunk: &[u8],
    ) -> Result<ResumeStep, AcpCoreError> {
        let mut session_result: Option<Map<String, Value>> = None;
        {
            let pending = self.pending_creates.get_mut(process_id).ok_or_else(|| {
                AcpCoreError::InvalidState(format!("no pending create_session for {process_id}"))
            })?;
            pending
                .stdout_buffer
                .push_str(&String::from_utf8_lossy(chunk));

            while let Some(idx) = pending.stdout_buffer.find('\n') {
                let line: String = pending.stdout_buffer.drain(..=idx).collect();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                match pending.step {
                    CreateStep::AwaitingInitialize => {
                        if message.get("id").and_then(Value::as_i64) != Some(1) {
                            continue;
                        }
                        let init = response_result(message, "ACP initialize")?;
                        validate_initialize_result(&init, pending.protocol_version)?;
                        let session_new_params = session_lifecycle_params(
                            &pending.cwd,
                            pending.mcp_servers.clone(),
                            &pending.additional_directories,
                            init.get("agentCapabilities"),
                        )?;
                        pending.init_result = Some(init);
                        let session_new = json!({
                            "jsonrpc": "2.0",
                            "id": 2,
                            "method": "session/new",
                            "params": session_new_params,
                        });
                        write_json_line(host, process_id, &session_new)?;
                        pending.step = CreateStep::AwaitingSessionNew;
                    }
                    CreateStep::AwaitingSessionNew => {
                        if message.get("id").and_then(Value::as_i64) != Some(2) {
                            continue;
                        }
                        session_result = Some(response_result(message, "ACP session/new")?);
                        break;
                    }
                }
            }
        }

        let Some(session_result) = session_result else {
            return Ok(ResumeStep::Pending);
        };

        // Handshake complete: build + record the session outside the pending borrow.
        let pending = self
            .pending_creates
            .remove(process_id)
            .expect("pending entry exists");
        let init_result = pending.init_result.clone().unwrap_or_default();
        let session_id = session_id_from_session_result(&session_result, process_id);
        host.bind_session(&session_id, process_id)?;
        let record = AcpSessionRecord {
            session_id: session_id.clone(),
            owner_connection_id: pending.owner_connection_id.clone(),
            agent_type: pending.agent_type.clone(),
            process_id: process_id.to_string(),
            pid: pending.pid,
            modes: optional_field_json(&session_result, &init_result, "modes"),
            config_options: config_options(&init_result, &session_result),
            agent_capabilities: optional_value_json(init_result.get("agentCapabilities")),
            agent_info: optional_value_json(init_result.get("agentInfo")),
            stdout_buffer: String::new(),
            next_request_id: 3,
            closed: false,
            exit_code: None,
            pending_preamble: None,
        };
        let response = AcpResponse::AcpSessionCreatedResponse(record.created_response());
        self.sessions.insert(session_id, record);
        Ok(ResumeStep::Done(response))
    }

    /// RESUMABLE `session/prompt` (and other in-session RPC) — start. Owner-only;
    /// allocates the rpc id, injects `sessionId`, consumes any armed preamble, writes
    /// the request, and RETURNS without waiting. The agent's reply (and its mid-turn
    /// syscalls — pi's inference is a `net` call here) are handled via
    /// `feed_agent_output` across separate, non-nested `pushFrame`s.
    pub fn begin_session_request<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpSessionRequest,
    ) -> Result<String, AcpCoreError> {
        let mut outbound_params = match request.params.as_deref() {
            Some(params) => to_record(parse_json_text(params, "session request params")?),
            None => Map::new(),
        };
        outbound_params.insert(
            String::from("sessionId"),
            Value::String(request.session_id.clone()),
        );

        let unknown =
            || AcpCoreError::InvalidState(format!("unknown ACP session {}", request.session_id));
        let session = self
            .sessions
            .get_mut(&request.session_id)
            .ok_or_else(unknown)?;
        if session.owner_connection_id != caller_connection_id {
            return Err(unknown());
        }
        let rpc_id = session.allocate_request_id();
        let pending_preamble = if request.method == "session/prompt" {
            session.pending_preamble.take()
        } else {
            None
        };
        let process_id = session.process_id.clone();
        if let Some(preamble) = pending_preamble.as_deref() {
            prepend_prompt_preamble(&mut outbound_params, preamble);
        }

        let outbound = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": request.method,
            "params": Value::Object(outbound_params),
        });
        write_json_line(host, &process_id, &outbound)?;

        self.pending_prompts.insert(
            process_id.clone(),
            PendingPrompt {
                session_id: request.session_id.clone(),
                rpc_id,
                stdout_buffer: String::new(),
            },
        );
        Ok(process_id)
    }

    fn feed_prompt(&mut self, process_id: &str, chunk: &[u8]) -> Result<ResumeStep, AcpCoreError> {
        let mut completed: Option<(String, String)> = None; // (session_id, response_text)
        {
            let pending = self
                .pending_prompts
                .get_mut(process_id)
                .expect("pending prompt exists");
            pending
                .stdout_buffer
                .push_str(&String::from_utf8_lossy(chunk));
            while let Some(idx) = pending.stdout_buffer.find('\n') {
                let line: String = pending.stdout_buffer.drain(..=idx).collect();
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                // Ignore notifications / other ids; only the matching response completes.
                if message.get("id").and_then(Value::as_i64) != Some(pending.rpc_id) {
                    continue;
                }
                let text = serde_json::to_string(&message).map_err(|error| {
                    AcpCoreError::InvalidState(format!(
                        "failed to serialize ACP session response: {error}"
                    ))
                })?;
                completed = Some((pending.session_id.clone(), text));
                break;
            }
        }
        let Some((session_id, response)) = completed else {
            return Ok(ResumeStep::Pending);
        };
        self.pending_prompts.remove(process_id);
        Ok(ResumeStep::Done(AcpResponse::AcpSessionRpcResponse(
            AcpSessionRpcResponse {
                session_id,
                response,
                event_count: 0,
            },
        )))
    }

    /// In-flight resumable interactions (create + prompt), for diagnostics/tests.
    pub fn pending_create_count(&self) -> usize {
        self.pending_creates.len()
    }
    pub fn pending_prompt_count(&self) -> usize {
        self.pending_prompts.len()
    }

    fn bootstrap_session<H: AcpHost>(
        &self,
        host: &mut H,
        request: &AcpCreateSessionRequest,
        process_id: &str,
    ) -> Result<SessionBootstrap, AcpCoreError> {
        let mut stdout = String::new();
        let client_capabilities =
            parse_json_text(&request.client_capabilities, "clientCapabilities")?;
        let mcp_servers = parse_json_text(&request.mcp_servers, "mcpServers")?;

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": request.protocol_version,
                "clientCapabilities": client_capabilities,
            },
        });
        let init_response = send_json_rpc(
            host,
            process_id,
            &initialize,
            1,
            Some(INITIALIZE_TIMEOUT_MS),
            &mut stdout,
        )?;
        let init_result = response_result(init_response, "ACP initialize")?;
        validate_initialize_result(&init_result, request.protocol_version)?;

        let session_new_params = session_lifecycle_params(
            &request.cwd,
            mcp_servers,
            &request.additional_directories,
            init_result.get("agentCapabilities"),
        )?;
        let session_new = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": session_new_params,
        });
        let session_response = send_json_rpc(
            host,
            process_id,
            &session_new,
            2,
            Some(SESSION_NEW_TIMEOUT_MS),
            &mut stdout,
        )?;
        let session_result = response_result(session_response, "ACP session/new")?;
        let session_id = session_id_from_session_result(&session_result, process_id);

        Ok(SessionBootstrap {
            session_id,
            modes: optional_field_json(&session_result, &init_result, "modes"),
            config_options: config_options(&init_result, &session_result),
            agent_capabilities: optional_value_json(init_result.get("agentCapabilities")),
            agent_info: optional_value_json(init_result.get("agentInfo")),
            stdout_buffer: stdout,
        })
    }

    /// In-session JSON-RPC (`session/prompt`, `session/set_mode`, `session/cancel`,
    /// etc.): owner-only, forwards the method+params to the live adapter over the
    /// seam and returns the agent's JSON-RPC response. Mirrors the native
    /// `session_request`.
    ///
    /// FOLLOW-UP (documented, parity-tracked): the native handler also (a) forwards
    /// adapter notifications emitted during the exchange as `AcpSessionEvent`s and
    /// synthesizes mode/plan notifications via `apply_request_success`, and (b)
    /// converts a `session/cancel` "method not found" into a `session/cancel`
    /// notification fallback. Those layer on the same loop once the host seam
    /// surfaces notifications; the core request/response path is faithful now.
    pub fn session_request<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpSessionRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        let mut outbound_params = match request.params.as_deref() {
            Some(params) => to_record(parse_json_text(params, "session request params")?),
            None => Map::new(),
        };
        outbound_params.insert(
            String::from("sessionId"),
            Value::String(request.session_id.clone()),
        );

        // Enforce per-connection ownership and allocate the rpc id BEFORE any
        // outbound write. A non-owner (or missing session) fails closed with the
        // SAME error as `get_session_state` so the victim session is not revealed
        // and no state is mutated on a rejected attempt.
        let unknown =
            || AcpCoreError::InvalidState(format!("unknown ACP session {}", request.session_id));
        let session = self
            .sessions
            .get_mut(&request.session_id)
            .ok_or_else(unknown)?;
        if session.owner_connection_id != caller_connection_id {
            return Err(unknown());
        }
        let rpc_id = session.allocate_request_id();
        // The transcript-continuation preamble is consumed once, on the first
        // `session/prompt` after a fallback resume; other methods leave it armed.
        let pending_preamble = if request.method == "session/prompt" {
            session.pending_preamble.take()
        } else {
            None
        };
        let process_id = session.process_id.clone();
        let mut stdout_buffer = std::mem::take(&mut session.stdout_buffer);

        if let Some(preamble) = pending_preamble.as_deref() {
            prepend_prompt_preamble(&mut outbound_params, preamble);
        }

        // ACP cancellation is notification-only. Do not occupy the serialized
        // adapter lane waiting for a response that conforming adapters never send;
        // higher layers determine quiescence from the prompt listener or hard-close
        // the session when that listener does not settle.
        if request.method == "session/cancel" {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": Value::Object(outbound_params),
            });
            write_json_line(host, &process_id, &notification)?;
            if let Some(session) = self.sessions.get_mut(&request.session_id) {
                session.stdout_buffer = stdout_buffer;
            }
            let response = json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": {
                    "cancelled": false,
                    "requested": true,
                    "via": "notification-fallback",
                },
            });
            return Ok(AcpResponse::AcpSessionRpcResponse(AcpSessionRpcResponse {
                session_id: request.session_id.clone(),
                response: response.to_string(),
                event_count: 0,
            }));
        }

        let outbound = json!({
            "jsonrpc": "2.0",
            "id": rpc_id,
            "method": request.method,
            "params": Value::Object(outbound_params),
        });
        let timeout = request_timeout_ms(&request.method);
        let response = match send_json_rpc(
            host,
            &process_id,
            &outbound,
            rpc_id,
            timeout,
            &mut stdout_buffer,
        ) {
            Ok(response) => response,
            Err(error) => {
                // Persist any drained stdout and re-arm the consumed preamble so a
                // transient failure does not silently drop transcript context.
                if let Some(session) = self.sessions.get_mut(&request.session_id) {
                    session.stdout_buffer = stdout_buffer;
                    if pending_preamble.is_some() && session.pending_preamble.is_none() {
                        session.pending_preamble = pending_preamble;
                    }
                }
                return Err(error);
            }
        };

        if let Some(session) = self.sessions.get_mut(&request.session_id) {
            session.stdout_buffer = stdout_buffer;
        }

        let response_text = serde_json::to_string(&response).map_err(|error| {
            AcpCoreError::InvalidState(format!("failed to serialize ACP session response: {error}"))
        })?;
        Ok(AcpResponse::AcpSessionRpcResponse(AcpSessionRpcResponse {
            session_id: request.session_id.clone(),
            response: response_text,
            event_count: 0,
        }))
    }

    /// `session/resume`: re-attach a session that exists in durable storage but is
    /// not live in this VM. Launches a fresh adapter, re-probes its capabilities via
    /// `initialize`, then tries the native `session/load`/`session/resume` tier and
    /// falls back to a fresh `session/new` (arming the transcript-continuation
    /// preamble) on the `unknown_session` sentinel. Mirrors the native
    /// `resume_session` state machine (spec §6/§8).
    pub fn resume_session<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: &AcpResumeSessionRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        let resolved = resolve_agent(host, &request.agent_type)?;

        let process_id = self.allocate_process_id("acp-agent");
        let mut env: BTreeMap<String, String> = request.env.clone().into_iter().collect();
        env.insert(String::from("AGENTOS_KEEP_STDIN_OPEN"), String::from("1"));
        env.insert(
            String::from("AGENTOS_EAGER_STDIN_HANDLE"),
            String::from("1"),
        );
        // Manifest env applies as DEFAULTS; caller/base env wins on conflicts.
        for (key, value) in &resolved.env {
            env.entry(key.clone()).or_insert_with(|| value.clone());
        }

        let spawned = host.spawn_agent(SpawnAgentRequest {
            process_id: process_id.clone(),
            runtime: AcpRuntimeKind::JavaScript,
            entrypoint: Some(resolved.entrypoint.clone()),
            command: None,
            args: resolved.launch_args.clone(),
            env,
            cwd: Some(request.cwd.clone()),
        })?;

        let outcome = match self.resume_bootstrap(host, request, &process_id) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = host.kill_agent(&process_id, "SIGKILL");
                return Err(error);
            }
        };

        let session = AcpSessionRecord {
            session_id: outcome.bootstrap.session_id.clone(),
            owner_connection_id: caller_connection_id.to_string(),
            agent_type: request.agent_type.clone(),
            process_id: process_id.clone(),
            pid: spawned.pid,
            modes: outcome.bootstrap.modes,
            config_options: outcome.bootstrap.config_options,
            agent_capabilities: outcome.bootstrap.agent_capabilities,
            agent_info: outcome.bootstrap.agent_info,
            stdout_buffer: outcome.bootstrap.stdout_buffer,
            next_request_id: 3,
            closed: false,
            exit_code: None,
            pending_preamble: outcome.pending_preamble,
        };

        host.bind_session(&session.session_id, &process_id)?;
        let response = AcpResponse::AcpSessionResumedResponse(AcpSessionResumedResponse {
            session_id: session.session_id.clone(),
            mode: outcome.mode,
        });
        self.sessions.insert(session.session_id.clone(), session);
        Ok(response)
    }

    fn resume_bootstrap<H: AcpHost>(
        &self,
        host: &mut H,
        request: &AcpResumeSessionRequest,
        process_id: &str,
    ) -> Result<ResumeOutcome, AcpCoreError> {
        let mut stdout = String::new();
        let client_capabilities =
            parse_json_text(DEFAULT_RESUME_CLIENT_CAPABILITIES, "clientCapabilities")?;

        let initialize = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": ACP_RESUME_PROTOCOL_VERSION,
                "clientCapabilities": client_capabilities,
            },
        });
        let init_response = send_json_rpc(
            host,
            process_id,
            &initialize,
            1,
            Some(INITIALIZE_TIMEOUT_MS),
            &mut stdout,
        )?;
        let init_result = response_result(init_response, "ACP initialize")?;
        validate_initialize_result(&init_result, ACP_RESUME_PROTOCOL_VERSION)?;
        let agent_capabilities = init_result.get("agentCapabilities").cloned();
        let mcp_servers = parse_json_text(&request.mcp_servers, "mcpServers")?;
        let lifecycle_params = session_lifecycle_params(
            &request.cwd,
            mcp_servers,
            &request.additional_directories,
            agent_capabilities.as_ref(),
        )?;

        // Tier 1 — native (capability-gated). Re-probed caps decide eligibility.
        if let Some(native_method) = native_resume_method(agent_capabilities.as_ref()) {
            let mut load_params = lifecycle_params.clone();
            load_params.insert(
                String::from("sessionId"),
                Value::String(request.session_id.clone()),
            );
            let load = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": native_method,
                "params": load_params,
            });
            let mut load_response = send_json_rpc(
                host,
                process_id,
                &load,
                2,
                Some(SESSION_NEW_TIMEOUT_MS),
                &mut stdout,
            )?;
            normalize_unknown_session_error(&mut load_response);
            if load_response.get("error").is_none() {
                let load_result = response_result(load_response, &format!("ACP {native_method}"))?;
                return Ok(ResumeOutcome {
                    bootstrap: self.build_bootstrap(
                        request.session_id.clone(),
                        &init_result,
                        &load_result,
                        agent_capabilities.as_ref(),
                        stdout,
                    ),
                    mode: String::from("native"),
                    pending_preamble: None,
                });
            }
            // Only the `unknown_session` sentinel falls through; every other error
            // propagates verbatim (the durable store survived; this is a real error).
            if !is_unknown_session_error(&load_response) {
                return Err(
                    response_result(load_response, &format!("ACP {native_method}"))
                        .expect_err("native resume error object must map to an AcpCoreError"),
                );
            }
            // fall through to Tier 2
        }

        // Tier 2 — universal fallback: a fresh session plus the transcript pointer.
        let session_new = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": lifecycle_params,
        });
        let session_response = send_json_rpc(
            host,
            process_id,
            &session_new,
            2,
            Some(SESSION_NEW_TIMEOUT_MS),
            &mut stdout,
        )?;
        let session_result = response_result(session_response, "ACP session/new")?;
        let live_session_id = session_id_from_session_result(&session_result, process_id);
        let pending_preamble = request
            .transcript_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .map(|path| CONTINUATION_PREAMBLE.replace("{path}", path));
        Ok(ResumeOutcome {
            bootstrap: self.build_bootstrap(
                live_session_id,
                &init_result,
                &session_result,
                agent_capabilities.as_ref(),
                stdout,
            ),
            mode: String::from("fallback"),
            pending_preamble,
        })
    }

    fn build_bootstrap(
        &self,
        session_id: String,
        init_result: &Map<String, Value>,
        session_result: &Map<String, Value>,
        agent_capabilities: Option<&Value>,
        stdout_buffer: String,
    ) -> SessionBootstrap {
        SessionBootstrap {
            session_id,
            modes: optional_field_json(session_result, init_result, "modes"),
            config_options: config_options(init_result, session_result),
            agent_capabilities: optional_value_json(agent_capabilities),
            agent_info: optional_value_json(init_result.get("agentInfo")),
            stdout_buffer,
        }
    }

    /// Dispatch a decoded ACP request to the right handler.
    pub fn dispatch<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: AcpRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        match request {
            AcpRequest::AcpCreateSessionRequest(request) => {
                self.create_session(host, caller_connection_id, &request)
            }
            AcpRequest::AcpGetSessionStateRequest(request) => {
                self.get_session_state(caller_connection_id, &request)
            }
            AcpRequest::AcpCloseSessionRequest(request) => {
                self.close_session(host, caller_connection_id, &request)
            }
            AcpRequest::AcpSessionRequest(request) => {
                self.session_request(host, caller_connection_id, &request)
            }
            AcpRequest::AcpResumeSessionRequest(request) => {
                self.resume_session(host, caller_connection_id, &request)
            }
            AcpRequest::AcpDeliverAgentOutputRequest(request) => {
                self.deliver_agent_output(host, &request)
            }
            // Agent enumeration needs a directory listing of `/opt/agentos`, which
            // the host-free `AcpHost` seam does not expose (it has `read_file`, not
            // `read_dir`). The native sidecar answers `list_agents` directly from the
            // projected packages; the browser resumable path does not support it yet.
            AcpRequest::AcpListAgentsRequest(_) => Err(AcpCoreError::InvalidState(
                "list_agents is not handled by the host-free ACP core; the native sidecar answers \
                 it from the projected /opt/agentos packages"
                    .to_string(),
            )),
            AcpRequest::AcpOpenSessionRequest(_)
            | AcpRequest::AcpGetDurableSessionRequest(_)
            | AcpRequest::AcpListDurableSessionsRequest(_)
            | AcpRequest::AcpDeleteSessionRequest(_)
            | AcpRequest::AcpUnloadSessionRequest(_)
            | AcpRequest::AcpPromptRequest(_)
            | AcpRequest::AcpCancelPromptRequest(_)
            | AcpRequest::AcpRespondPermissionRequest(_)
            | AcpRequest::AcpReadHistoryRequest(_)
            | AcpRequest::AcpGetSessionConfigRequest(_)
            | AcpRequest::AcpSetSessionConfigOptionRequest(_)
            | AcpRequest::AcpGetSessionCapabilitiesRequest(_)
            | AcpRequest::AcpGetSessionAgentInfoRequest(_) => Err(
                AcpCoreError::InvalidState(
                    "durable sessions require native sidecar VM SQLite and are not supported by the dormant browser ACP core"
                        .to_string(),
                ),
            ),
        }
    }

    /// RESUMABLE dispatch (browser path): `create_session` / `session/prompt` start a
    /// non-blocking handshake and return [`AcpPendingResponse`] with the process
    /// handle; `deliver_agent_output` feeds the agent's stdout and returns the real
    /// result once the handshake completes (else another `AcpPendingResponse`).
    /// Everything else (get_state / close / resume) is delegated to the synchronous
    /// handlers. This is what the in-worker kernel calls so it never block-waits
    /// inside `pushFrame` while an agent makes a mid-turn syscall (§3.2.1).
    pub fn dispatch_resumable<H: AcpHost>(
        &mut self,
        host: &mut H,
        caller_connection_id: &str,
        request: AcpRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        match request {
            AcpRequest::AcpCreateSessionRequest(request) => {
                let process_id = self.begin_create_session(host, caller_connection_id, &request)?;
                Ok(AcpResponse::AcpPendingResponse(AcpPendingResponse {
                    process_id,
                }))
            }
            AcpRequest::AcpSessionRequest(request) => {
                let process_id =
                    self.begin_session_request(host, caller_connection_id, &request)?;
                Ok(AcpResponse::AcpPendingResponse(AcpPendingResponse {
                    process_id,
                }))
            }
            AcpRequest::AcpDeliverAgentOutputRequest(request) => {
                self.deliver_agent_output(host, &request)
            }
            other => self.dispatch(host, caller_connection_id, other),
        }
    }

    fn deliver_agent_output<H: AcpHost>(
        &mut self,
        host: &mut H,
        request: &AcpDeliverAgentOutputRequest,
    ) -> Result<AcpResponse, AcpCoreError> {
        match self.feed_agent_output(host, &request.process_id, &request.chunk)? {
            ResumeStep::Pending => Ok(AcpResponse::AcpPendingResponse(AcpPendingResponse {
                process_id: request.process_id.clone(),
            })),
            ResumeStep::Done(response) => Ok(response),
        }
    }
}

/// Outcome of the resume handshake: the bootstrap state plus the chosen tier mode
/// (`native`/`fallback`) and any armed transcript-continuation preamble.
struct ResumeOutcome {
    bootstrap: SessionBootstrap,
    mode: String,
    pending_preamble: Option<String>,
}

// ---- host-free helpers ported from agentos-sidecar::acp_extension ----

/// Coerce a parsed JSON value into an object map (a non-object becomes empty), so
/// session-request params are always a JSON object we can inject `sessionId` into.
fn to_record(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Per-method JSON-RPC timeout. Mirrors the native `request_timeout`.
fn request_timeout_ms(method: &str) -> Option<u64> {
    match method {
        "session/prompt" => None,
        "initialize" => Some(INITIALIZE_TIMEOUT_MS),
        "session/new" => Some(SESSION_NEW_TIMEOUT_MS),
        _ => Some(120_000),
    }
}

/// Prepend the transcript-continuation preamble as a leading text block on a
/// `session/prompt`'s `prompt` array (initialized if absent). Mirrors the native
/// `prepend_prompt_preamble`.
fn prepend_prompt_preamble(params: &mut Map<String, Value>, preamble: &str) {
    let block = json!({ "type": "text", "text": preamble });
    match params.get_mut("prompt").and_then(Value::as_array_mut) {
        Some(prompt) => prompt.insert(0, block),
        None => {
            params.insert(String::from("prompt"), Value::Array(vec![block]));
        }
    }
}

fn session_capability(agent_capabilities: Option<&Value>, name: &str) -> bool {
    agent_capabilities
        .and_then(Value::as_object)
        .and_then(|caps| caps.get("sessionCapabilities"))
        .and_then(Value::as_object)
        .and_then(|caps| caps.get(name))
        .is_some_and(Value::is_object)
}

fn serialized_session_capability(
    agent_capabilities: Option<&str>,
    name: &str,
) -> Result<bool, AcpCoreError> {
    agent_capabilities
        .map(|capabilities| {
            parse_json_text(capabilities, "agentCapabilities")
                .map(|capabilities| session_capability(Some(&capabilities), name))
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn session_lifecycle_params(
    cwd: &str,
    mcp_servers: Value,
    additional_directories: &[String],
    agent_capabilities: Option<&Value>,
) -> Result<Map<String, Value>, AcpCoreError> {
    if additional_directories.len() > MAX_ACP_ADDITIONAL_DIRECTORIES {
        return Err(AcpCoreError::InvalidState(format!(
            "ACP additionalDirectories exceeds {MAX_ACP_ADDITIONAL_DIRECTORIES} entries"
        )));
    }
    for (index, directory) in additional_directories.iter().enumerate() {
        if !directory.starts_with('/') {
            return Err(AcpCoreError::InvalidState(format!(
                "ACP additionalDirectories[{index}] must be an absolute guest path"
            )));
        }
        if directory.len() > MAX_ACP_GUEST_PATH_BYTES {
            return Err(AcpCoreError::InvalidState(format!(
                "ACP additionalDirectories[{index}] exceeds {MAX_ACP_GUEST_PATH_BYTES} bytes"
            )));
        }
    }
    if !additional_directories.is_empty()
        && !session_capability(agent_capabilities, "additionalDirectories")
    {
        return Err(AcpCoreError::InvalidState(String::from(
            "ACP agent does not advertise sessionCapabilities.additionalDirectories",
        )));
    }

    let mut params = Map::from_iter([
        (String::from("cwd"), Value::String(cwd.to_string())),
        (String::from("mcpServers"), mcp_servers),
    ]);
    if !additional_directories.is_empty() {
        params.insert(
            String::from("additionalDirectories"),
            Value::Array(
                additional_directories
                    .iter()
                    .cloned()
                    .map(Value::String)
                    .collect(),
            ),
        );
    }
    Ok(params)
}

/// The adapter's native-resume RPC method from re-probed `agentCapabilities`:
/// prefer ACP `session/load`, then stable `sessionCapabilities.resume`. Mirrors
/// the native `native_resume_method` and retains the pre-standard boolean draft.
fn native_resume_method(agent_capabilities: Option<&Value>) -> Option<&'static str> {
    let caps = agent_capabilities.and_then(Value::as_object)?;
    if caps
        .get("loadSession")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some("session/load");
    }
    if session_capability(agent_capabilities, "resume")
        || caps.get("resume").and_then(Value::as_bool).unwrap_or(false)
    {
        return Some("session/resume");
    }
    None
}

/// Normalize an adapter "no such session" error (`-32603` + `details ==
/// "NotFoundError"`) into the shared `unknown_session` discriminator. Strict on
/// purpose: malformed `session/load` must still propagate. Mirrors the native
/// `normalize_unknown_session_error`.
fn normalize_unknown_session_error(response: &mut Value) {
    let Some(error) = response.get_mut("error").and_then(Value::as_object_mut) else {
        return;
    };
    let code = error.get("code").and_then(Value::as_i64);
    let Some(data) = error.get_mut("data").and_then(Value::as_object_mut) else {
        return;
    };
    let details = data.get("details").and_then(Value::as_str);
    if code == Some(-32603) && details == Some("NotFoundError") {
        data.insert(
            String::from("kind"),
            Value::String(String::from("unknown_session")),
        );
    }
}

/// Detect the normalized `unknown_session` fallthrough sentinel. Only this triggers
/// the Tier 2 fallback; transport/timeout errors propagate. Mirrors the native
/// `is_unknown_session_error`.
fn is_unknown_session_error(response: &Value) -> bool {
    response
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| error.get("data"))
        .and_then(Value::as_object)
        .and_then(|d| d.get("kind"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "unknown_session")
}

/// True when a signal/kill request failed because the target process no longer
/// exists — the process-table `ESRCH` / "no such process" / "has no active
/// process" errors surfaced for an already-reaped PID. `close_session` uses
/// this to skip the exit wait when the adapter is already gone. Mirrors the
/// native sidecar's `is_process_already_gone_error`.
fn is_process_already_gone_error(error: &AcpCoreError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("esrch")
        || message.contains("no such process")
        || message.contains("has no active process")
}

/// Write a JSON-RPC message as a single newline-terminated line to the agent's
/// stdin (no waiting). Used by the resumable handshake.
fn write_json_line<H: AcpHost>(
    host: &mut H,
    process_id: &str,
    message: &Value,
) -> Result<(), AcpCoreError> {
    let mut line = serde_json::to_vec(message).map_err(|error| {
        AcpCoreError::InvalidState(format!("failed to serialize ACP request: {error}"))
    })?;
    line.push(b'\n');
    host.write_stdin(process_id, &line)
}

fn parse_json_text(text: &str, label: &str) -> Result<Value, AcpCoreError> {
    serde_json::from_str(text)
        .map_err(|error| AcpCoreError::InvalidState(format!("invalid {label} JSON: {error}")))
}

fn response_result(response: Value, label: &str) -> Result<Map<String, Value>, AcpCoreError> {
    if let Some(error) = response.get("error").and_then(Value::as_object) {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown ACP error");
        let data = error
            .get("data")
            .map(|d| format!(" (data: {d})"))
            .unwrap_or_default();
        return Err(AcpCoreError::Execution(format!(
            "{label} failed: {message}{data}"
        )));
    }
    response
        .get("result")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| AcpCoreError::InvalidState(format!("{label} response missing result")))
}

fn validate_initialize_result(
    result: &Map<String, Value>,
    requested_protocol_version: i32,
) -> Result<(), AcpCoreError> {
    let reported = result
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            AcpCoreError::InvalidState(String::from(
                "ACP initialize response missing protocolVersion",
            ))
        })?;
    if reported != i64::from(requested_protocol_version) {
        return Err(AcpCoreError::InvalidState(format!(
            "ACP initialize protocolVersion mismatch: requested {requested_protocol_version}, agent reported {reported}"
        )));
    }
    Ok(())
}

fn session_id_from_session_result(session_result: &Map<String, Value>, fallback: &str) -> String {
    session_result
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|session_id| !session_id.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback.to_string())
}

/// JSON-encode a present, non-null value to its string form (the record stores
/// these fields as opaque JSON text, decoded client-side).
fn optional_value_json(value: Option<&Value>) -> Option<String> {
    value
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::to_string(value).ok())
}

/// Prefer the session/new result, fall back to initialize, for an optional field.
fn optional_field_json(
    primary: &Map<String, Value>,
    fallback: &Map<String, Value>,
    key: &str,
) -> Option<String> {
    optional_value_json(primary.get(key).or_else(|| fallback.get(key)))
}

/// Config options: prefer session/new's array, else initialize's; each element is
/// stored as its JSON-text form.
fn config_options(
    init_result: &Map<String, Value>,
    session_result: &Map<String, Value>,
) -> Vec<String> {
    let array = session_result
        .get("configOptions")
        .and_then(Value::as_array)
        .or_else(|| init_result.get("configOptions").and_then(Value::as_array));
    array
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::to_string(item).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentOutput, SpawnAgentRequest, SpawnedAgent};

    #[test]
    fn prompt_has_no_deadline_while_bootstrap_close_and_machine_rpcs_remain_bounded() {
        assert_eq!(request_timeout_ms("session/prompt"), None);
        assert_eq!(request_timeout_ms("initialize"), Some(60_000));
        assert_eq!(request_timeout_ms("session/new"), Some(120_000));
        assert_eq!(request_timeout_ms("session/set_mode"), Some(120_000));
        assert_eq!(SESSION_CLOSE_TIMEOUT_MS, 5_000);
    }

    #[test]
    fn stable_resume_and_additional_directory_capabilities_are_detected() {
        let capabilities = json!({
            "sessionCapabilities": {
                "resume": {},
                "additionalDirectories": {},
            }
        });
        assert_eq!(
            native_resume_method(Some(&capabilities)),
            Some("session/resume")
        );
        let params = session_lifecycle_params(
            "/workspace",
            json!([]),
            &[String::from("/reference")],
            Some(&capabilities),
        )
        .expect("advertised additional directories");
        assert_eq!(params["additionalDirectories"], json!(["/reference"]));
    }

    #[derive(Default)]
    struct MockHost {
        killed: Vec<(String, String)>,
        closed_stdin: Vec<String>,
    }

    impl AcpHost for MockHost {
        fn spawn_agent(&mut self, _: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError> {
            unreachable!("non-process handlers do not spawn")
        }
        fn bind_session(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn write_stdin(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
            unreachable!()
        }
        fn close_stdin(&mut self, process_id: &str) -> Result<(), AcpCoreError> {
            self.closed_stdin.push(process_id.to_string());
            Ok(())
        }
        fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
            Ok(None)
        }
        fn kill_agent(&mut self, process_id: &str, signal: &str) -> Result<(), AcpCoreError> {
            self.killed
                .push((process_id.to_string(), signal.to_string()));
            Ok(())
        }
        fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
            Ok(Some(0)) // exits promptly after SIGTERM
        }
        fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
            Ok(br#"{"name":"echo","agent":{"acpEntrypoint":"echo-agent"}}"#.to_vec())
        }
        fn now_ms(&self) -> u64 {
            0
        }
    }

    fn record(session_id: &str, owner: &str) -> AcpSessionRecord {
        AcpSessionRecord {
            session_id: session_id.into(),
            owner_connection_id: owner.into(),
            agent_type: "echo".into(),
            process_id: format!("proc-{session_id}"),
            pid: Some(42),
            modes: None,
            config_options: Vec::new(),
            agent_capabilities: None,
            agent_info: None,
            stdout_buffer: String::new(),
            next_request_id: 1,
            closed: false,
            exit_code: None,
            pending_preamble: None,
        }
    }

    #[test]
    fn get_session_state_enforces_ownership() {
        let mut core = AcpCore::new();
        core.insert_session(record("s1", "conn-a"));
        let req = AcpGetSessionStateRequest {
            session_id: "s1".into(),
        };
        // Owner reads it.
        assert!(core.get_session_state("conn-a", &req).is_ok());
        // Non-owner gets the same "unknown" error (no cross-tenant leak).
        let err = core.get_session_state("conn-b", &req).unwrap_err();
        assert_eq!(err.code(), "invalid_state");
        assert!(err.to_string().contains("unknown ACP session"));
    }

    #[test]
    fn close_session_owner_only_and_kills_process() {
        let mut core = AcpCore::new();
        core.insert_session(record("s1", "conn-a"));
        let mut host = MockHost::default();
        let req = AcpCloseSessionRequest {
            session_id: "s1".into(),
        };
        // Non-owner cannot close.
        assert!(core.close_session(&mut host, "conn-b", &req).is_err());
        assert_eq!(core.session_count(), 1);
        // Owner closes: process is torn down and the record removed.
        let resp = core
            .close_session(&mut host, "conn-a", &req)
            .expect("close");
        assert!(matches!(resp, AcpResponse::AcpSessionClosedResponse(_)));
        assert_eq!(core.session_count(), 0);
        assert_eq!(host.closed_stdin, vec!["proc-s1".to_string()]);
        assert_eq!(host.killed, vec![("proc-s1".into(), "SIGTERM".into())]);
    }

    /// Host whose adapter process is already gone: `wait_for_exit` would time
    /// out (returns `None`), so any wait the engine performs is dead time. The
    /// wait counter proves the short-circuit: a regression re-enters the
    /// SIGTERM → wait → SIGKILL → wait sequence and records 2 waits.
    #[derive(Default)]
    struct GoneAdapterHost {
        kill_error: Option<String>,
        killed: Vec<(String, String)>,
        waits: usize,
    }

    impl AcpHost for GoneAdapterHost {
        fn spawn_agent(&mut self, _: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError> {
            unreachable!("close_session does not spawn")
        }
        fn bind_session(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn write_stdin(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
            unreachable!()
        }
        fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
            Ok(None)
        }
        fn kill_agent(&mut self, process_id: &str, signal: &str) -> Result<(), AcpCoreError> {
            self.killed
                .push((process_id.to_string(), signal.to_string()));
            match &self.kill_error {
                Some(message) => Err(AcpCoreError::InvalidState(message.clone())),
                None => Ok(()),
            }
        }
        fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
            self.waits += 1;
            Ok(None) // the exit event was already drained; a wait can only time out
        }
        fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
            Ok(Vec::new())
        }
        fn now_ms(&self) -> u64 {
            0
        }
    }

    #[test]
    fn close_session_skips_teardown_wait_when_session_already_closed() {
        let mut core = AcpCore::new();
        let mut dead = record("s1", "conn-a");
        dead.closed = true;
        dead.exit_code = Some(137);
        core.insert_session(dead);
        let mut host = GoneAdapterHost::default();
        let resp = core
            .close_session(
                &mut host,
                "conn-a",
                &AcpCloseSessionRequest {
                    session_id: "s1".into(),
                },
            )
            .expect("close");
        assert!(matches!(resp, AcpResponse::AcpSessionClosedResponse(_)));
        assert_eq!(core.session_count(), 0);
        // Already-observed exit: no signals sent, no dead waiting.
        assert!(host.killed.is_empty());
        assert_eq!(host.waits, 0);
    }

    #[test]
    fn close_session_skips_teardown_wait_when_sigterm_reports_process_gone() {
        let mut core = AcpCore::new();
        core.insert_session(record("s1", "conn-a"));
        let mut host = GoneAdapterHost {
            kill_error: Some(String::from("process proc-s1: no such process (ESRCH)")),
            ..GoneAdapterHost::default()
        };
        let resp = core
            .close_session(
                &mut host,
                "conn-a",
                &AcpCloseSessionRequest {
                    session_id: "s1".into(),
                },
            )
            .expect("close");
        assert!(matches!(resp, AcpResponse::AcpSessionClosedResponse(_)));
        // SIGTERM was attempted, classified as process-gone, and both waits
        // (and the SIGKILL escalation) were skipped.
        assert_eq!(host.killed, vec![("proc-s1".into(), "SIGTERM".into())]);
        assert_eq!(host.waits, 0);
    }

    #[test]
    fn session_request_enforces_ownership_without_side_effects() {
        // A non-owner prompt fails closed with the same unknown-session error and
        // does NOT consume a request id or touch the victim's state.
        let mut core = AcpCore::new();
        core.insert_session(record("s1", "conn-a"));
        let mut host = MockHost::default();
        let req = AcpSessionRequest {
            session_id: "s1".into(),
            method: "session/prompt".into(),
            params: None,
        };
        let err = core.session_request(&mut host, "conn-b", &req).unwrap_err();
        assert_eq!(err.code(), "invalid_state");
        assert!(err.to_string().contains("unknown ACP session"));
        // next_request_id untouched (no id consumed on the rejected attempt).
        assert_eq!(core.sessions.get("s1").unwrap().next_request_id, 1);
    }

    #[test]
    fn session_request_round_trips_a_prompt_through_the_agent() {
        use serde_json::Value;
        use std::collections::VecDeque;

        // A host whose agent answers session/prompt with a stopReason, echoing the
        // rpc id the core allocated.
        #[derive(Default)]
        struct PromptHost {
            out: VecDeque<AgentOutput>,
            clock: u64,
            last_request: Option<Value>,
        }
        impl AcpHost for PromptHost {
            fn spawn_agent(&mut self, _: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError> {
                unreachable!()
            }
            fn bind_session(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn write_stdin(&mut self, _: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
                let request: Value =
                    serde_json::from_slice(chunk.strip_suffix(b"\n").unwrap_or(chunk)).unwrap();
                let id = request["id"].as_i64().unwrap();
                self.last_request = Some(request);
                let reply = json!({"jsonrpc":"2.0","id":id,"result":{"stopReason":"end_turn"}});
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                self.out.push_back(AgentOutput::Stdout(bytes));
                Ok(())
            }
            fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
                self.clock += 1;
                Ok(self.out.pop_front())
            }
            fn kill_agent(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
                Ok(Some(0))
            }
            fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
                Ok(br#"{"name":"echo","agent":{"acpEntrypoint":"echo-agent"}}"#.to_vec())
            }
            fn now_ms(&self) -> u64 {
                self.clock
            }
        }

        let mut core = AcpCore::new();
        core.insert_session(record("s1", "conn-a"));
        let mut host = PromptHost::default();
        let req = AcpSessionRequest {
            session_id: "s1".into(),
            method: "session/prompt".into(),
            params: Some(r#"{"prompt":[{"type":"text","text":"hi"}]}"#.into()),
        };

        let response = core
            .session_request(&mut host, "conn-a", &req)
            .expect("prompt round-trip");
        match response {
            AcpResponse::AcpSessionRpcResponse(rpc) => {
                assert_eq!(rpc.session_id, "s1");
                let body: Value = serde_json::from_str(&rpc.response).unwrap();
                assert_eq!(body["result"]["stopReason"], json!("end_turn"));
            }
            other => panic!("expected rpc response, got {other:?}"),
        }
        // The core injected sessionId into the outbound params and consumed an id.
        let sent = host.last_request.unwrap();
        assert_eq!(sent["params"]["sessionId"], json!("s1"));
        assert_eq!(core.sessions.get("s1").unwrap().next_request_id, 2);
    }

    #[test]
    fn resume_falls_back_to_session_new_when_no_native_capability() {
        use agentos_protocol::generated::v1::{AcpResumeSessionRequest, AcpRuntimeKind};
        use serde_json::Value;
        use std::collections::{HashMap, VecDeque};

        // An agent advertising NO loadSession/resume cap: resume must take Tier 2
        // (session/new) and arm the transcript preamble.
        #[derive(Default)]
        struct ResumeHost {
            out: VecDeque<AgentOutput>,
            clock: u64,
        }
        impl AcpHost for ResumeHost {
            fn spawn_agent(
                &mut self,
                request: SpawnAgentRequest,
            ) -> Result<SpawnedAgent, AcpCoreError> {
                Ok(SpawnedAgent {
                    process_id: request.process_id,
                    pid: Some(9),
                })
            }
            fn bind_session(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn write_stdin(&mut self, _: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
                let request: Value =
                    serde_json::from_slice(chunk.strip_suffix(b"\n").unwrap_or(chunk)).unwrap();
                let id = request["id"].as_i64().unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    // No agentCapabilities -> native_resume_method returns None.
                    "initialize" => json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1}}),
                    "session/new" => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"live-1"}})
                    }
                    other => panic!("unexpected method {other}"),
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                self.out.push_back(AgentOutput::Stdout(bytes));
                Ok(())
            }
            fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
                self.clock += 1;
                Ok(self.out.pop_front())
            }
            fn kill_agent(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
                Ok(Some(0))
            }
            fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
                Ok(br#"{"name":"echo","agent":{"acpEntrypoint":"echo-agent"}}"#.to_vec())
            }
            fn now_ms(&self) -> u64 {
                self.clock
            }
        }

        let mut core = AcpCore::new();
        let mut host = ResumeHost::default();
        let request = AcpResumeSessionRequest {
            session_id: "old-session".into(),
            agent_type: "echo".into(),
            runtime: AcpRuntimeKind::JavaScript,
            transcript_path: Some("/transcripts/old.jsonl".into()),
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            mcp_servers: "[]".into(),
            env: HashMap::new(),
        };

        let response = core
            .resume_session(&mut host, "conn-a", &request)
            .expect("resume");
        match response {
            AcpResponse::AcpSessionResumedResponse(resumed) => {
                assert_eq!(resumed.session_id, "live-1");
                assert_eq!(resumed.mode, "fallback");
            }
            other => panic!("expected resumed response, got {other:?}"),
        }
        // The fallback armed the transcript-continuation preamble for the next prompt.
        let preamble = core
            .sessions
            .get("live-1")
            .unwrap()
            .pending_preamble
            .clone()
            .expect("preamble armed");
        assert!(preamble.contains("/transcripts/old.jsonl"));
    }

    #[test]
    fn create_session_runs_the_acp_handshake_round_trip() {
        use agentos_protocol::generated::v1::{AcpCreateSessionRequest, AcpRuntimeKind};
        use serde_json::{json, Value};
        use std::collections::{HashMap, VecDeque};

        // A mock that spawns and answers the ACP handshake (initialize + session/new)
        // like a minimal ACP echo agent.
        #[derive(Default)]
        struct CreateHost {
            out: VecDeque<AgentOutput>,
            clock: u64,
            bound: Vec<(String, String)>,
        }
        impl AcpHost for CreateHost {
            fn spawn_agent(
                &mut self,
                request: SpawnAgentRequest,
            ) -> Result<SpawnedAgent, AcpCoreError> {
                Ok(SpawnedAgent {
                    process_id: request.process_id,
                    pid: Some(7),
                })
            }
            fn bind_session(
                &mut self,
                session_id: &str,
                process_id: &str,
            ) -> Result<(), AcpCoreError> {
                self.bound.push((session_id.into(), process_id.into()));
                Ok(())
            }
            fn write_stdin(&mut self, _: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
                let request: Value =
                    serde_json::from_slice(chunk.strip_suffix(b"\n").unwrap_or(chunk)).unwrap();
                let id = request["id"].as_i64().unwrap();
                let reply = match request["method"].as_str().unwrap() {
                    "initialize" => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"protocolVersion":1,"agentInfo":{"name":"echo"}}})
                    }
                    "session/new" => {
                        json!({"jsonrpc":"2.0","id":id,"result":{"sessionId":"sess-xyz"}})
                    }
                    other => panic!("unexpected method {other}"),
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(b'\n');
                self.out.push_back(AgentOutput::Stdout(bytes));
                Ok(())
            }
            fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
                self.clock += 1;
                Ok(self.out.pop_front())
            }
            fn kill_agent(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
                Ok(Some(0))
            }
            fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
                Ok(())
            }
            fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
                Ok(br#"{"name":"echo","agent":{"acpEntrypoint":"echo-agent"}}"#.to_vec())
            }
            fn now_ms(&self) -> u64 {
                self.clock
            }
        }

        let mut core = AcpCore::new();
        let mut host = CreateHost::default();
        let request = AcpCreateSessionRequest {
            agent_type: "echo".into(),
            runtime: AcpRuntimeKind::JavaScript,
            protocol_version: 1,
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            args: Vec::new(),
            env: HashMap::new(),
            client_capabilities: "{}".into(),
            mcp_servers: "[]".into(),
            additional_instructions: None,
            skip_os_instructions: false,
        };

        let response = core
            .create_session(&mut host, "conn-a", &request)
            .expect("create session");
        match response {
            AcpResponse::AcpSessionCreatedResponse(created) => {
                assert_eq!(created.session_id, "sess-xyz");
            }
            other => panic!("expected created response, got {other:?}"),
        }
        assert_eq!(core.session_count(), 1);
        assert_eq!(host.bound, vec![("sess-xyz".into(), "acp-agent-0".into())]);

        // The freshly-created session is readable by its owner (full round-trip:
        // create -> state).
        let state = core
            .get_session_state(
                "conn-a",
                &AcpGetSessionStateRequest {
                    session_id: "sess-xyz".into(),
                },
            )
            .expect("state");
        assert!(matches!(state, AcpResponse::AcpSessionStateResponse(_)));
    }

    // ---- resumable (browser, non-blocking) create_session ----

    use agentos_protocol::generated::v1::{AcpCreateSessionRequest, AcpRuntimeKind};
    use std::collections::HashMap;

    /// A host that records stdin writes but NEVER produces output on its own — the
    /// resumable path is driven entirely by feed_agent_output, so poll_output must
    /// never be called (it would block forever in the blocking path).
    #[derive(Default)]
    struct ResumableMockHost {
        stdin: Vec<String>,
        bound: Vec<(String, String)>,
    }
    impl AcpHost for ResumableMockHost {
        fn spawn_agent(
            &mut self,
            request: SpawnAgentRequest,
        ) -> Result<SpawnedAgent, AcpCoreError> {
            Ok(SpawnedAgent {
                process_id: request.process_id,
                pid: Some(11),
            })
        }
        fn bind_session(&mut self, session_id: &str, process_id: &str) -> Result<(), AcpCoreError> {
            self.bound.push((session_id.into(), process_id.into()));
            Ok(())
        }
        fn write_stdin(&mut self, _: &str, chunk: &[u8]) -> Result<(), AcpCoreError> {
            self.stdin
                .push(String::from_utf8_lossy(chunk).trim().to_string());
            Ok(())
        }
        fn close_stdin(&mut self, _: &str) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn poll_output(&mut self, _: &str) -> Result<Option<AgentOutput>, AcpCoreError> {
            unreachable!("resumable path must not poll_output (it never blocks)")
        }
        fn kill_agent(&mut self, _: &str, _: &str) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn wait_for_exit(&mut self, _: &str, _: u64) -> Result<Option<i32>, AcpCoreError> {
            Ok(Some(0))
        }
        fn write_file(&mut self, _: &str, _: &[u8]) -> Result<(), AcpCoreError> {
            Ok(())
        }
        fn read_file(&mut self, _: &str) -> Result<Vec<u8>, AcpCoreError> {
            Ok(br#"{"name":"echo","agent":{"acpEntrypoint":"echo-agent"}}"#.to_vec())
        }
        fn now_ms(&self) -> u64 {
            0
        }
    }

    fn echo_create_request() -> AcpCreateSessionRequest {
        AcpCreateSessionRequest {
            agent_type: "echo".into(),
            runtime: AcpRuntimeKind::JavaScript,
            protocol_version: 1,
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            args: Vec::new(),
            env: HashMap::new(),
            client_capabilities: "{}".into(),
            mcp_servers: "[]".into(),
            additional_instructions: None,
            skip_os_instructions: false,
        }
    }

    #[test]
    fn resumable_create_session_drives_the_handshake_without_blocking() {
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();

        // begin: spawns + writes initialize, returns immediately, no session yet.
        let process_id = core
            .begin_create_session(&mut host, "conn-a", &echo_create_request())
            .expect("begin");
        assert_eq!(core.session_count(), 0);
        assert_eq!(core.pending_create_count(), 1);
        assert!(host.stdin[0].contains("\"method\":\"initialize\""));

        // feed the initialize response → still pending, session/new now written.
        let step = core
            .feed_agent_output(
                &mut host,
                &process_id,
                br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"echo"}}}
"#,
            )
            .expect("feed initialize");
        assert!(matches!(step, ResumeStep::Pending));
        assert!(host.stdin[1].contains("\"method\":\"session/new\""));
        assert_eq!(core.session_count(), 0);

        // feed the session/new response → Created.
        let step = core
            .feed_agent_output(
                &mut host,
                &process_id,
                br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-xyz"}}
"#,
            )
            .expect("feed session/new");
        match step {
            ResumeStep::Done(AcpResponse::AcpSessionCreatedResponse(created)) => {
                assert_eq!(created.session_id, "sess-xyz");
            }
            other => panic!("expected Created, got {other:?}"),
        }
        assert_eq!(core.session_count(), 1);
        assert_eq!(core.pending_create_count(), 0);
        assert_eq!(host.bound, vec![("sess-xyz".into(), "acp-agent-0".into())]);

        // The created session is queryable by its owner (full resumable round-trip).
        let state = core
            .get_session_state(
                "conn-a",
                &AcpGetSessionStateRequest {
                    session_id: "sess-xyz".into(),
                },
            )
            .expect("state");
        assert!(matches!(state, AcpResponse::AcpSessionStateResponse(_)));
    }

    #[test]
    fn resumable_create_session_buffers_partial_lines() {
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();
        let process_id = core
            .begin_create_session(&mut host, "conn-a", &echo_create_request())
            .expect("begin");

        // Deliver the initialize response in three chunks split mid-line.
        let init = br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}
"#;
        let (a, rest) = init.split_at(10);
        let (b, c) = rest.split_at(20);
        assert!(matches!(
            core.feed_agent_output(&mut host, &process_id, a)
                .expect("a"),
            ResumeStep::Pending
        ));
        assert_eq!(host.stdin.len(), 1, "no full line yet → no session/new");
        assert!(matches!(
            core.feed_agent_output(&mut host, &process_id, b)
                .expect("b"),
            ResumeStep::Pending
        ));
        assert!(matches!(
            core.feed_agent_output(&mut host, &process_id, c)
                .expect("c"),
            ResumeStep::Pending
        ));
        // Only once the newline arrives is the line parsed and session/new written.
        assert!(host.stdin[1].contains("session/new"));

        let step = core
            .feed_agent_output(
                &mut host,
                &process_id,
                br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"chunked-1"}}
"#,
            )
            .expect("feed session/new");
        match step {
            ResumeStep::Done(AcpResponse::AcpSessionCreatedResponse(c)) => {
                assert_eq!(c.session_id, "chunked-1")
            }
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[test]
    fn resumable_create_session_propagates_an_agent_initialize_error() {
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();
        let process_id = core
            .begin_create_session(&mut host, "conn-a", &echo_create_request())
            .expect("begin");
        let err = core
            .feed_agent_output(
                &mut host,
                &process_id,
                br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"boom"}}
"#,
            )
            .expect_err("initialize error must surface");
        assert_eq!(err.code(), "execution");
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn resumable_session_prompt_drives_a_prompt_without_blocking() {
        use agentos_protocol::generated::v1::AcpSessionRequest;
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();

        // Bring a session live via the resumable create path.
        let process_id = core
            .begin_create_session(&mut host, "conn-a", &echo_create_request())
            .expect("begin create");
        core.feed_agent_output(
            &mut host,
            &process_id,
            br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}
"#,
        )
        .expect("init");
        core.feed_agent_output(
            &mut host,
            &process_id,
            br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess-p"}}
"#,
        )
        .expect("session/new");
        assert_eq!(core.session_count(), 1);
        host.stdin.clear();

        // begin a resumable prompt: writes the prompt, returns immediately.
        let prompt = AcpSessionRequest {
            session_id: "sess-p".into(),
            method: "session/prompt".into(),
            params: Some(r#"{"prompt":[{"type":"text","text":"hi"}]}"#.into()),
        };
        let prompt_process = core
            .begin_session_request(&mut host, "conn-a", &prompt)
            .expect("begin prompt");
        assert_eq!(prompt_process, process_id);
        assert_eq!(core.pending_prompt_count(), 1);
        // sessionId injected; rpc id is 3 (next after the create handshake's 1,2).
        assert!(host.stdin[0].contains("\"method\":\"session/prompt\""));
        assert!(host.stdin[0].contains("\"sessionId\":\"sess-p\""));
        assert!(host.stdin[0].contains("\"id\":3"));

        // feed the prompt response → Done with the agent's reply.
        let step = core
            .feed_agent_output(
                &mut host,
                &process_id,
                br#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}
"#,
            )
            .expect("feed prompt response");
        match step {
            ResumeStep::Done(AcpResponse::AcpSessionRpcResponse(rpc)) => {
                assert_eq!(rpc.session_id, "sess-p");
                let body: Value = serde_json::from_str(&rpc.response).unwrap();
                assert_eq!(body["result"]["stopReason"], json!("end_turn"));
            }
            other => panic!("expected rpc Done, got {other:?}"),
        }
        assert_eq!(core.pending_prompt_count(), 0);
    }

    #[test]
    fn dispatch_resumable_drives_create_session_over_the_wire_types() {
        use agentos_protocol::generated::v1::AcpDeliverAgentOutputRequest;
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();

        // create_session via the resumable dispatch → AcpPendingResponse with the handle.
        let pending = core
            .dispatch_resumable(
                &mut host,
                "conn-a",
                AcpRequest::AcpCreateSessionRequest(echo_create_request()),
            )
            .expect("begin");
        let process_id = match pending {
            AcpResponse::AcpPendingResponse(p) => p.process_id,
            other => panic!("expected pending, got {other:?}"),
        };

        // deliver the initialize response → still pending.
        let step = core
            .dispatch_resumable(
                &mut host,
                "conn-a",
                AcpRequest::AcpDeliverAgentOutputRequest(AcpDeliverAgentOutputRequest {
                    process_id: process_id.clone(),
                    chunk: br#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}
"#
                    .to_vec(),
                }),
            )
            .expect("deliver init");
        assert!(matches!(step, AcpResponse::AcpPendingResponse(_)));

        // deliver the session/new response → the real created result.
        let step = core
            .dispatch_resumable(
                &mut host,
                "conn-a",
                AcpRequest::AcpDeliverAgentOutputRequest(AcpDeliverAgentOutputRequest {
                    process_id: process_id.clone(),
                    chunk: br#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"wire-sess"}}
"#
                    .to_vec(),
                }),
            )
            .expect("deliver session/new");
        match step {
            AcpResponse::AcpSessionCreatedResponse(created) => {
                assert_eq!(created.session_id, "wire-sess")
            }
            other => panic!("expected created, got {other:?}"),
        }
        assert_eq!(core.session_count(), 1);
    }

    #[test]
    fn resumable_session_prompt_enforces_ownership() {
        use agentos_protocol::generated::v1::AcpSessionRequest;
        let mut core = AcpCore::new();
        let mut host = ResumableMockHost::default();
        core.insert_session(record("s1", "conn-a"));
        let req = AcpSessionRequest {
            session_id: "s1".into(),
            method: "session/prompt".into(),
            params: None,
        };
        let err = core
            .begin_session_request(&mut host, "conn-b", &req)
            .expect_err("non-owner must be rejected");
        assert_eq!(err.code(), "invalid_state");
        assert_eq!(core.pending_prompt_count(), 0);
    }
}
