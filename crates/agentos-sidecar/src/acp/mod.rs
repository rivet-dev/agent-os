use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use agent_client_protocol_schema::v1::{McpServer, NewSessionRequest};
use agentos_native_sidecar::extension::ExtensionSnapshot;
use agentos_native_sidecar::limits::AcpLimits;
#[cfg(test)]
use agentos_native_sidecar::limits::DEFAULT_ACP_MAX_READ_LINE_BYTES;
use agentos_native_sidecar::wire::{
    CloseStdinRequest, EventPayload, ExecuteRequest, KillProcessRequest, OwnershipScope,
    StreamChannel, WriteStdinRequest,
};
use agentos_native_sidecar::{
    Extension, ExtensionContext, ExtensionFuture, ExtensionInterruptRequest,
    ExtensionInterruptResponse, ExtensionResponse, SidecarError,
};
use agentos_protocol::generated::v1::*;
use agentos_protocol::ACP_EXTENSION_NAMESPACE;
use agentos_runtime::accounting::{LimitError, ResourceClass};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::session_store::{
    timestamp, PendingRequestResolution, SessionStore, StoredEvent, StoredSession,
    StoredSessionSummary,
};

mod restore;
mod runtime;
mod turn;

use restore::*;
// Re-exported only for the standalone regression that textually loads this
// module and exercises the production timeout selector.
#[allow(unused_imports)]
pub(crate) use runtime::request_timeout;
use runtime::*;
use turn::*;

// Cold Node adapters can spend tens of seconds loading their module graph and
// opening their local database on a contended host. Keep both bootstrap phases
// bounded by one attempt without imposing a shorter deadline on either phase.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_NEW_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const ACP_MACHINE_HOST_CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);
// Long-running turns and human-mediated permission waits are not failures.
// Warn after sustained inactivity, then repeat at the same conservative cadence
// without imposing a deadline or changing the request's outcome.
const ACP_INACTIVITY_WARNING_INTERVAL: Duration = Duration::from_secs(30);
// Some ACP adapters resolve `session/prompt` before their final session/update
// notification reaches stdout. Keep the transport attached for one short quiet
// window so the prompt result and its durable tool history stay atomic.
const PROMPT_RESPONSE_DRAIN_QUIET: Duration = Duration::from_millis(50);
// While an ACP request is in flight the stdio loop is inside the extension
// dispatch, so this wait loop becomes the cooperative VM I/O pump. Keep it at
// the same cadence as agentos's outer event pump so adapter fetches and
// process output keep moving mid-turn.
const ACP_CANCEL_METHOD: &str = "session/cancel";
const ACP_TRACE_PATH_ENV: &str = "AGENT_OS_ACP_TRACE_PATH";
/// ACP protocol version used for the resume handshake. Lockstep single version.
const ACP_RESUME_PROTOCOL_VERSION: i32 = 1;
/// Client capabilities advertised during the resume `initialize`. Mirrors the
/// client's `defaultAcpClientCapabilities()` so resumed sessions behave like
/// freshly created ones.
const DEFAULT_RESUME_CLIENT_CAPABILITIES: &str =
    "{\"fs\":{\"readTextFile\":true,\"writeTextFile\":true},\"terminal\":true}";
/// Adapter-neutral contract between the shared ACP runtime and an
/// AgentOS-owned package launcher. The launcher translates this text into the
/// upstream adapter's native flag, SDK option, or context-file mechanism.
const ACP_APPEND_SYSTEM_PROMPT_ENV: &str = "ACP_APPEND_SYSTEM_PROMPT";
// Embedded next to this source so `cargo publish` packages it (an out-of-crate
// `include_str!` path breaks the isolated package-verify build). The TypeScript
// side reads the same file from this location for its sanity check.
const AGENTOS_SYSTEM_PROMPT: &str = include_str!("../AGENTOS_SYSTEM_PROMPT.md");
/// Substring identifying the `send_json_rpc_request` error raised when the
/// adapter process exits before answering. `session_request` matches on it to
/// evict the now-dead session record instead of leaking it until an explicit
/// internal runtime cleanup that may never run.
const ADAPTER_EXITED_ERROR_MARKER: &str = "exited with code";
/// Substring of the agentos process-table error returned when an operation
/// targets a process that already exited ("VM <vm> has no active process <id>").
/// Writing a request to an adapter that crashed while *idle* surfaces this way
/// (the exit is observed lazily, on the next stdin write), so it is classified
/// as an adapter-gone failure alongside `ADAPTER_EXITED_ERROR_MARKER`.
const ADAPTER_NO_ACTIVE_PROCESS_MARKER: &str = "has no active process";
/// `AcpAgentExitedEvent.restart` outcome for the native runtime. AgentOS never
/// respawns adapters or replays requests implicitly; restoration is an
/// explicit session operation initiated by the caller.
const ADAPTER_RESTART_OUTCOME_NOT_ATTEMPTED: &str = "not_attempted";

#[derive(Debug)]
struct InactivityWarnings {
    started_at: Instant,
    last_activity_at: Instant,
    last_activity: String,
    next_warning_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
struct InactivityWarning {
    elapsed: Duration,
    inactive: Duration,
    last_activity_elapsed: Duration,
    last_activity: String,
}

impl InactivityWarnings {
    fn new(description: impl Into<String>) -> Self {
        Self::new_at(description, Instant::now())
    }

    fn new_at(description: impl Into<String>, now: Instant) -> Self {
        Self {
            started_at: now,
            last_activity_at: now,
            last_activity: description.into(),
            next_warning_at: now + ACP_INACTIVITY_WARNING_INTERVAL,
        }
    }

    fn record(&mut self, description: impl Into<String>) {
        self.record_at(description, Instant::now());
    }

    fn record_at(&mut self, description: impl Into<String>, now: Instant) {
        self.last_activity_at = now;
        self.last_activity = description.into();
        self.next_warning_at = now + ACP_INACTIVITY_WARNING_INTERVAL;
    }

    fn wait_duration(&self, now: Instant) -> Duration {
        self.next_warning_at.saturating_duration_since(now)
    }

    fn take_due(&mut self, now: Instant) -> Option<InactivityWarning> {
        if now < self.next_warning_at {
            return None;
        }
        self.next_warning_at = now + ACP_INACTIVITY_WARNING_INTERVAL;
        Some(InactivityWarning {
            elapsed: now.saturating_duration_since(self.started_at),
            inactive: now.saturating_duration_since(self.last_activity_at),
            last_activity_elapsed: self
                .last_activity_at
                .saturating_duration_since(self.started_at),
            last_activity: self.last_activity.clone(),
        })
    }
}

#[derive(Debug, Default)]
pub struct AcpExtension {
    next_process_id: AtomicUsize,
    sessions: Mutex<BTreeMap<String, LiveAcpRuntime>>,
    prompt_cancellations: StdMutex<BTreeMap<String, tokio::sync::watch::Sender<bool>>>,
    pending_permission_responses: Arc<StdMutex<BTreeMap<String, PendingPermissionResponse>>>,
}

#[derive(Debug)]
struct PendingPermissionResponse {
    offered_option_ids: BTreeSet<String>,
    acp_request_id: Value,
    sender: tokio::sync::oneshot::Sender<PendingPermissionSignal>,
}

#[derive(Debug)]
enum PendingPermissionSignal {
    Selected(String),
    Terminal(String),
}

#[derive(Debug, Clone)]
struct LiveAcpRuntime {
    acp_session_id: String,
    /// Stable AgentOS session ID when this live ACP route backs durable state.
    user_session_id: Option<String>,
    /// Connection that created this session. Used to enforce per-connection
    /// ownership so one connection cannot read or drive another connection's
    /// ACP session by its `session_id`.
    owner_connection_id: String,
    agent_type: String,
    process_id: String,
    pid: Option<u32>,
    modes: Option<String>,
    config_options: Vec<String>,
    agent_capabilities: Option<String>,
    agent_info: Option<String>,
    stdout_buffer: String,
    next_request_id: i64,
    closed: bool,
    /// Set by the resume fallback tier (`session/new` instead of native
    /// `session/load`). The transcript-continuation preamble is prepended, once,
    /// as a leading text content block on this session's next `session/prompt`,
    /// then cleared. See the resume state machine on
    /// `AcpExtension::restore_acp_runtime`.
    pending_preamble: Option<String>,
}

impl AcpExtension {
    pub fn new() -> Self {
        Self::default()
    }

    async fn handle_payload(
        &self,
        mut ctx: ExtensionContext<'_>,
        payload: &[u8],
    ) -> Result<ExtensionResponse, SidecarError> {
        use tracing::Instrument as _;
        let request = decode_request(payload)?;
        let kind = Self::acp_request_kind(&request);
        let start = std::time::Instant::now();
        tracing::info!(target: "agentos_sidecar::acp_extension", kind, "ext request received");

        let work = async move {
            match request {
                AcpRequest::AcpOpenSessionRequest(request) => {
                    self.open_session(&mut ctx, request).await
                }
                AcpRequest::AcpGetDurableSessionRequest(request) => {
                    AcpHandlerOutput::response(self.get_durable_session(&mut ctx, request).await)
                }
                AcpRequest::AcpListDurableSessionsRequest(request) => {
                    AcpHandlerOutput::response(self.list_durable_sessions(&mut ctx, request).await)
                }
                AcpRequest::AcpDeleteSessionRequest(request) => {
                    self.delete_durable_session(&mut ctx, request).await
                }
                AcpRequest::AcpUnloadSessionRequest(request) => {
                    self.unload_durable_session(&mut ctx, request).await
                }
                AcpRequest::AcpPromptRequest(request) => {
                    self.prompt_durable_session(&mut ctx, request).await
                }
                AcpRequest::AcpCancelPromptRequest(request) => {
                    self.cancel_durable_prompt(&mut ctx, request).await
                }
                AcpRequest::AcpRespondPermissionRequest(request) => {
                    self.respond_durable_permission(&mut ctx, request).await
                }
                AcpRequest::AcpReadHistoryRequest(request) => {
                    AcpHandlerOutput::response(self.read_history(&mut ctx, request).await)
                }
                AcpRequest::AcpGetSessionConfigRequest(request) => {
                    AcpHandlerOutput::response(self.get_durable_config(&mut ctx, request).await)
                }
                AcpRequest::AcpSetSessionConfigOptionRequest(request) => {
                    self.set_durable_config(&mut ctx, request).await
                }
                AcpRequest::AcpGetSessionCapabilitiesRequest(request) => {
                    AcpHandlerOutput::response(self.get_durable_capabilities(&mut ctx, request).await)
                }
                AcpRequest::AcpGetSessionAgentInfoRequest(request) => {
                    AcpHandlerOutput::response(self.get_durable_agent_info(&mut ctx, request).await)
                }
                AcpRequest::AcpListAgentsRequest(_) => self.list_agents(ctx).await,
                AcpRequest::AcpCreateSessionRequest(_)
                | AcpRequest::AcpGetSessionStateRequest(_)
                | AcpRequest::AcpCloseSessionRequest(_)
                | AcpRequest::AcpSessionRequest(_)
                | AcpRequest::AcpResumeSessionRequest(_) => AcpHandlerOutput::response(Err(
                    SidecarError::Unsupported(String::from(
                        "legacy live-session RPC removed; use the durable session API",
                    )),
                )),
                AcpRequest::AcpDeliverAgentOutputRequest(_) => AcpHandlerOutput::response(Err(
                    SidecarError::InvalidState(
                        "AcpDeliverAgentOutputRequest is dispatched by the engine/browser resumable path, not the native ACP extension".to_string(),
                    ),
                )),
            }
        }
        .instrument(tracing::info_span!(
            target: "agentos_sidecar::acp_extension",
            "ext.request",
            kind
        ));

        let response = work.await;

        tracing::info!(
            target: "agentos_sidecar::acp_extension",
            kind,
            elapsed_ms = start.elapsed().as_millis() as u64,
            "ext request handled",
        );
        let payload = encode_response(response.response.unwrap_or_else(error_response))?;
        ExtensionResponse::with_wire_events(payload, response.events)
    }

    /// Stable label for an ACP request kind, used as a tracing field.
    fn acp_request_kind(request: &AcpRequest) -> &'static str {
        match request {
            AcpRequest::AcpCreateSessionRequest(_) => "legacy_create_session",
            AcpRequest::AcpOpenSessionRequest(_) => "open_session",
            AcpRequest::AcpGetDurableSessionRequest(_) => "get_session",
            AcpRequest::AcpListDurableSessionsRequest(_) => "list_sessions",
            AcpRequest::AcpDeleteSessionRequest(_) => "delete_session",
            AcpRequest::AcpUnloadSessionRequest(_) => "unload_session",
            AcpRequest::AcpPromptRequest(_) => "prompt",
            AcpRequest::AcpCancelPromptRequest(_) => "cancel_prompt",
            AcpRequest::AcpRespondPermissionRequest(_) => "respond_permission",
            AcpRequest::AcpReadHistoryRequest(_) => "read_history",
            AcpRequest::AcpGetSessionConfigRequest(_) => "get_session_config",
            AcpRequest::AcpSetSessionConfigOptionRequest(_) => "set_session_config_option",
            AcpRequest::AcpGetSessionCapabilitiesRequest(_) => "get_session_capabilities",
            AcpRequest::AcpGetSessionAgentInfoRequest(_) => "get_session_agent_info",
            AcpRequest::AcpGetSessionStateRequest(_) => "legacy_get_session_state",
            AcpRequest::AcpCloseSessionRequest(_) => "legacy_close_session",
            AcpRequest::AcpSessionRequest(_) => "legacy_session_request",
            AcpRequest::AcpResumeSessionRequest(_) => "legacy_resume_session",
            AcpRequest::AcpListAgentsRequest(_) => "list_agents",
            AcpRequest::AcpDeliverAgentOutputRequest(_) => "deliver_agent_output",
        }
    }

    async fn session_store(
        &self,
        ctx: &mut ExtensionContext<'_>,
    ) -> Result<SessionStore, SidecarError> {
        let limits = ctx.vm_acp_limits().await?;
        let database = ctx.vm_database().await?.ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "session_storage_unavailable: VM was created without a database descriptor",
            ))
        })?;
        Ok(SessionStore::from_database(database).with_limits(&limits))
    }

    async fn open_session(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpOpenSessionRequest,
    ) -> AcpHandlerOutput {
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let session_id = request
            .session_id
            .clone()
            .unwrap_or_else(|| String::from("main"));
        if let Err(error) = validate_user_session_id(&session_id) {
            return AcpHandlerOutput::response(Err(error));
        }
        let cwd = request
            .cwd
            .clone()
            .unwrap_or_else(|| String::from("/home/agentos"));
        let creation_options = match canonical_creation_options(&request, &cwd) {
            Ok(options) => options,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        match store.get(&session_id).await {
            Ok(Some(existing)) => {
                if existing.agent != request.agent
                    || existing.creation_options_json != creation_options
                {
                    return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                        "session_conflict: session {session_id} already exists with different immutable creation options"
                    ))));
                }
                return match self.ensure_durable_runtime(ctx, &store, existing).await {
                    Ok(_) => AcpHandlerOutput::response(Ok(AcpResponse::AcpOpenSessionResponse(
                        AcpOpenSessionResponse { reserved: false },
                    ))),
                    Err(error) => AcpHandlerOutput::response(Err(error)),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return AcpHandlerOutput::response(Err(session_store_error(error)));
            }
        }

        let env = match request
            .env
            .as_deref()
            .map(|value| parse_string_map(value, "env"))
            .transpose()
        {
            Ok(env) => env.unwrap_or_default(),
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let additional_directories = match request
            .additional_directories
            .as_deref()
            .map(|value| parse_json_text(value, "additionalDirectories"))
            .transpose()
        {
            Ok(Some(value)) => match serde_json::from_value::<Vec<PathBuf>>(value) {
                Ok(value) => value,
                Err(error) => {
                    return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                        "invalid additionalDirectories: {error}"
                    ))))
                }
            },
            Ok(None) => Vec::new(),
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let mcp_servers = match request
            .mcp_servers
            .as_deref()
            .map(parse_mcp_servers)
            .transpose()
        {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let mcp_servers_json = match serde_json::to_string(&mcp_servers) {
            Ok(value) => value,
            Err(error) => {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                    "failed to serialize validated ACP MCP servers: {error}"
                ))));
            }
        };
        let create = AcpCreateSessionRequest {
            agent_type: request.agent.clone(),
            runtime: AcpRuntimeKind::JavaScript,
            cwd: cwd.clone(),
            additional_directories: additional_directories
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            args: Vec::new(),
            env,
            protocol_version: ACP_RESUME_PROTOCOL_VERSION,
            client_capabilities: DEFAULT_RESUME_CLIENT_CAPABILITIES.to_owned(),
            mcp_servers: mcp_servers_json,
            skip_os_instructions: request.skip_os_instructions.unwrap_or(false),
            additional_instructions: request.additional_instructions.clone(),
        };
        let route_key = durable_route_key(ctx.ownership(), &session_id);
        let created = self
            .start_acp_runtime(ctx, create, &session_id, &route_key, additional_directories)
            .await;
        let AcpHandlerOutput {
            response: Ok(AcpResponse::AcpSessionCreatedResponse(created)),
            events: _bootstrap_events,
        } = created
        else {
            return created;
        };
        let config_options_json = match json_strings_to_array_text(&created.config_options) {
            Ok(value) => value,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        if let Err(error) = store
            .create(
                &session_id,
                &request.agent,
                &created.session_id,
                &cwd,
                &creation_options,
                created.agent_capabilities.as_deref(),
                created.agent_info.as_deref(),
                &config_options_json,
            )
            .await
        {
            if let Err(cleanup_error) = self.stop_acp_runtime(ctx, &route_key).await {
                tracing::error!(
                    target: "agentos_sidecar::acp_extension",
                    session_id,
                    error = %cleanup_error,
                    "failed to clean up ACP runtime after session storage failure"
                );
            }
            return AcpHandlerOutput::response(Err(session_store_error(error)));
        }
        AcpHandlerOutput::response(Ok(AcpResponse::AcpOpenSessionResponse(
            AcpOpenSessionResponse { reserved: false },
        )))
    }

    async fn get_durable_session(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpGetDurableSessionRequest,
    ) -> Result<AcpResponse, SidecarError> {
        let session_id = default_session_id(request.session_id)?;
        let session = required_stored_session(&self.session_store(ctx).await?, &session_id).await?;
        stored_session_response(session, |session| {
            AcpResponse::AcpGetDurableSessionResponse(AcpGetDurableSessionResponse { session })
        })
    }

    async fn list_durable_sessions(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpListDurableSessionsRequest,
    ) -> Result<AcpResponse, SidecarError> {
        const DEFAULT_LIMIT: usize = 50;
        let max_limit = ctx.vm_acp_limits().await?.max_session_list_entries;
        let limit =
            usize::try_from(request.limit.unwrap_or(DEFAULT_LIMIT as u32)).unwrap_or(usize::MAX);
        if limit == 0 || limit > max_limit {
            return Err(SidecarError::InvalidState(format!(
                "session_list_limit: limit must be 1..={max_limit}; raise limits.acp.maxSessionListEntries to request a larger page"
            )));
        }
        let cursor = request
            .cursor
            .as_deref()
            .map(decode_list_cursor)
            .transpose()?;
        let mut sessions = self
            .session_store(ctx)
            .await?
            .list(cursor, limit + 1)
            .await
            .map_err(session_store_error)?;
        let has_more = sessions.len() > limit;
        sessions.truncate(limit);
        let next_cursor = if has_more {
            sessions.last().map(encode_list_cursor).transpose()?
        } else {
            None
        };
        let sessions = sessions
            .into_iter()
            .map(stored_session_summary_info)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AcpResponse::AcpListDurableSessionsResponse(
            AcpListDurableSessionsResponse {
                sessions,
                next_cursor,
            },
        ))
    }

    async fn delete_durable_session(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpDeleteSessionRequest,
    ) -> AcpHandlerOutput {
        let session_id = match default_session_id(request.session_id) {
            Ok(id) => id,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let stored = match store.get(&session_id).await {
            Ok(stored) => stored,
            Err(error) => return AcpHandlerOutput::response(Err(session_store_error(error))),
        };
        let route_key = durable_route_key(ctx.ownership(), &session_id);
        if self.sessions.lock().await.contains_key(&route_key) {
            if let Err(error) = self.stop_acp_runtime(ctx, &route_key).await {
                return AcpHandlerOutput::response(Err(SidecarError::InvalidState(format!(
                    "session_delete_cleanup_failed: session {} was retained because its adapter could not be stopped: {error}",
                    session_id
                ))));
            }
        }
        let _ = stored;
        match store.delete(&session_id).await {
            Ok(()) => AcpHandlerOutput::response(Ok(AcpResponse::AcpDeleteSessionResponse(
                AcpDeleteSessionResponse { reserved: false },
            ))),
            Err(error) => AcpHandlerOutput::response(Err(session_store_error(error))),
        }
    }

    async fn unload_durable_session(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpUnloadSessionRequest,
    ) -> AcpHandlerOutput {
        let session_id = match default_session_id(request.session_id) {
            Ok(id) => id,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let store = match self.session_store(ctx).await {
            Ok(store) => store,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let stored = match required_stored_session(&store, &session_id).await {
            Ok(session) => session,
            Err(error) => return AcpHandlerOutput::response(Err(error)),
        };
        let route_key = durable_route_key(ctx.ownership(), &session_id);
        if self.sessions.lock().await.contains_key(&route_key) {
            if let Err(error) = self.stop_acp_runtime(ctx, &route_key).await {
                return AcpHandlerOutput::response(Err(error));
            }
        }
        let _ = stored;
        AcpHandlerOutput::response(Ok(AcpResponse::AcpUnloadSessionResponse(
            AcpUnloadSessionResponse { reserved: false },
        )))
    }

    async fn read_history(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpReadHistoryRequest,
    ) -> Result<AcpResponse, SidecarError> {
        const DEFAULT_LIMIT: usize = 100;
        let max_limit = ctx.vm_acp_limits().await?.max_history_page_entries;
        if request.before.is_some() && request.after.is_some() {
            return Err(SidecarError::InvalidState(String::from(
                "invalid_history_cursor: before and after are mutually exclusive",
            )));
        }
        let limit =
            usize::try_from(request.limit.unwrap_or(DEFAULT_LIMIT as u32)).unwrap_or(usize::MAX);
        if limit == 0 || limit > max_limit {
            return Err(SidecarError::InvalidState(format!(
                "history_limit: limit must be 1..={max_limit}; raise limits.acp.maxHistoryPageEntries to request more"
            )));
        }
        let session_id = default_session_id(request.session_id)?;
        let store = self.session_store(ctx).await?;
        let session = store
            .enforce_history_retention(&session_id)
            .await
            .map_err(session_store_error)?
            .ok_or_else(|| {
                SidecarError::InvalidState(format!("session_not_found: {session_id}"))
            })?;
        let before = request.before.map(safe_sequence).transpose()?;
        let after = request.after.map(safe_sequence).transpose()?;
        if let Some(after) = after {
            if after.saturating_add(1) < session.oldest_retained_sequence {
                return Err(SidecarError::InvalidState(format!(
                    "history_cursor_expired: earliestAvailableSequence={}",
                    session.oldest_retained_sequence
                )));
            }
        }
        let page = store
            .read_history(&session, before, after, limit)
            .await
            .map_err(session_store_error)?;
        let events = page
            .events
            .into_iter()
            .map(|event| {
                Ok(AcpDurableHistoryEntry {
                    session_id: session_id.clone(),
                    sequence: u64::try_from(event.sequence).map_err(|_| {
                        SidecarError::InvalidState(String::from("invalid stored history sequence"))
                    })?,
                    timestamp: timestamp(event.occurred_at_ms).map_err(session_store_error)?,
                    event: decode_durable_event(&event.event_json)?,
                })
            })
            .collect::<Result<Vec<_>, SidecarError>>()?;
        Ok(AcpResponse::AcpHistoryPageResponse(
            AcpHistoryPageResponse {
                events,
                has_more_before: page.has_more_before,
                has_more_after: page.has_more_after,
            },
        ))
    }

    async fn get_durable_config(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpGetSessionConfigRequest,
    ) -> Result<AcpResponse, SidecarError> {
        let id = default_session_id(request.session_id)?;
        let session = required_stored_session(&self.session_store(ctx).await?, &id).await?;
        Ok(AcpResponse::AcpSessionConfigResponse(
            AcpSessionConfigResponse {
                revision: u64::try_from(session.config_revision).unwrap_or(0),
                options: session.config_options_json,
            },
        ))
    }

    async fn get_durable_capabilities(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpGetSessionCapabilitiesRequest,
    ) -> Result<AcpResponse, SidecarError> {
        let id = default_session_id(request.session_id)?;
        let session = required_stored_session(&self.session_store(ctx).await?, &id).await?;
        Ok(AcpResponse::AcpSessionCapabilitiesResponse(
            AcpSessionCapabilitiesResponse {
                capabilities: session.capabilities_json,
            },
        ))
    }

    async fn get_durable_agent_info(
        &self,
        ctx: &mut ExtensionContext<'_>,
        request: AcpGetSessionAgentInfoRequest,
    ) -> Result<AcpResponse, SidecarError> {
        let id = default_session_id(request.session_id)?;
        let session = required_stored_session(&self.session_store(ctx).await?, &id).await?;
        Ok(AcpResponse::AcpSessionAgentInfoResponse(
            AcpSessionAgentInfoResponse {
                agent_info: session.agent_info_json,
            },
        ))
    }
}
impl Extension for AcpExtension {
    fn namespace(&self) -> &str {
        ACP_EXTENSION_NAMESPACE
    }

    fn handle_request<'a>(
        &'a self,
        ctx: ExtensionContext<'a>,
        payload: Vec<u8>,
    ) -> ExtensionFuture<'a, ExtensionResponse> {
        Box::pin(async move {
            let response = self.handle_payload(ctx, &payload).await?;
            Ok(response)
        })
    }

    fn bootstrap_vm_database<'a>(
        &'a self,
        database: agentos_native_sidecar::vm_sqlite::SharedVmSqliteDatabase,
    ) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let store = SessionStore::open(database)
                .await
                .map_err(session_store_error)?;
            store
                .reconcile_interrupted_turns()
                .await
                .map_err(session_store_error)
        })
    }

    fn is_blocking_request(&self, payload: &[u8]) -> bool {
        matches!(
            decode_request(payload),
            Ok(AcpRequest::AcpSessionRequest(request)) if request.method == "session/prompt"
        ) || matches!(decode_request(payload), Ok(AcpRequest::AcpPromptRequest(_)))
    }

    fn on_dispose<'a>(&'a self) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            // Extension/sidecar teardown: drop every remaining session record so
            // no `stdout_buffer` survives the host process. The adapter processes
            // themselves are reaped by the host's own session/VM dispose; this
            // only frees the wrapper-side tracking map.
            self.sessions.lock().await.clear();
            Ok(())
        })
    }

    fn on_session_disposed<'a>(&'a self, ctx: ExtensionSnapshot) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            // The host invokes this only on DisposeReason::ConnectionClosed, i.e.
            // the client disconnected before durable orchestration stopped each live
            // session. Evict this connection's ACP session records — including
            // their potentially large `stdout_buffer` — so they don't outlive the
            // connection. This closes the disconnect path of H4 (the per-request
            // process-exit eviction and `on_dispose` cover the other paths).
            let connection_id = ownership_connection_id(ctx.ownership());
            self.cleanup_sessions_for_connection(&connection_id).await;
            Ok(())
        })
    }

    fn interrupt_blocking_request(
        &self,
        blocking_payload: &[u8],
        interrupt: ExtensionInterruptRequest<'_>,
    ) -> Option<ExtensionInterruptResponse> {
        let blocking = decode_request(blocking_payload).ok()?;
        if let AcpRequest::AcpPromptRequest(blocking_request) = blocking {
            let user_session_id = blocking_request
                .session_id
                .unwrap_or_else(|| String::from("main"));
            return match interrupt {
                ExtensionInterruptRequest::KillProcess => Some(ExtensionInterruptResponse {
                    interrupt_active: true,
                    interrupted_response_payload: encode_durable_interrupted_prompt(
                        &user_session_id,
                    )?,
                    interrupting_response_payload: None,
                }),
                ExtensionInterruptRequest::ExtensionPayload { payload, ownership } => {
                    match decode_request(payload).ok()? {
                        AcpRequest::AcpCancelPromptRequest(cancel) => {
                            let cancel_session_id =
                                cancel.session_id.unwrap_or_else(|| String::from("main"));
                            if cancel_session_id != user_session_id {
                                return None;
                            }
                            let key = durable_route_key(ownership, &user_session_id);
                            let signalled = self.signal_prompt_cancellation(&key);
                            // Permission response and prompt cancellation share
                            // the same registry lock, so exactly one wins. A
                            // cancelled permission waiter receives an explicit
                            // ACP cancelled outcome immediately.
                            self.cancel_pending_permissions(&key, "prompt_cancelled");
                            Some(ExtensionInterruptResponse {
                                interrupt_active: false,
                                interrupted_response_payload: encode_durable_interrupted_prompt(
                                    &user_session_id,
                                )?,
                                interrupting_response_payload: Some(
                                    encode_durable_cancel_response(signalled)?,
                                ),
                            })
                        }
                        AcpRequest::AcpRespondPermissionRequest(response) => {
                            let response_session_id = response.session_id;
                            if response_session_id != user_session_id {
                                return None;
                            }
                            let key = format!(
                                "{}:{}",
                                durable_route_key(ownership, &user_session_id),
                                response.request_id
                            );
                            let selected_option_id = response.option_id.clone();
                            // Validate before acknowledging the interrupt. An invalid
                            // option must not consume the live request and prevent the
                            // caller from correcting its response.
                            let (accepted, valid_options) = match self
                                .pending_permission_responses
                                .lock()
                            {
                                Ok(mut pending) => {
                                    if let Some(entry) = pending.get(&key) {
                                        if entry.offered_option_ids.contains(&response.option_id) {
                                            (pending.remove(&key), None)
                                        } else {
                                            (
                                                None,
                                                Some(
                                                    entry
                                                        .offered_option_ids
                                                        .iter()
                                                        .cloned()
                                                        .collect::<Vec<_>>(),
                                                ),
                                            )
                                        }
                                    } else {
                                        (None, None)
                                    }
                                }
                                Err(_) => {
                                    eprintln!("ERR_AGENTOS_PERMISSION_RESPONSE: permission response registry is poisoned");
                                    (None, None)
                                }
                            };
                            let accepted = accepted.is_some_and(|entry| {
                                entry
                                    .sender
                                    .send(PendingPermissionSignal::Selected(selected_option_id))
                                    .is_ok()
                            });
                            Some(ExtensionInterruptResponse {
                                interrupt_active: false,
                                interrupted_response_payload: encode_durable_interrupted_prompt(
                                    &user_session_id,
                                )?,
                                interrupting_response_payload: Some(
                                    if let Some(valid_options) = valid_options {
                                        encode_response(AcpResponse::AcpErrorResponse(AcpErrorResponse {
                                            code: String::from("invalid_permission_option"),
                                            message: format!(
                                                "invalid_permission_option: request {} does not offer {}; valid option IDs: {}",
                                                response.request_id,
                                                response.option_id,
                                                valid_options.join(", ")
                                            ),
                                        })).ok()?
                                    } else {
                                        encode_durable_permission_response(accepted)?
                                    },
                                ),
                            })
                        }
                        AcpRequest::AcpUnloadSessionRequest(unload)
                            if unload.session_id.as_deref().unwrap_or("main")
                                == user_session_id =>
                        {
                            let key = durable_route_key(ownership, &user_session_id);
                            self.cancel_pending_permissions(&key, "prompt_cancelled");
                            self.signal_prompt_cancellation(&key);
                            // Returning None deliberately queues the unload behind
                            // the active prompt. The signal makes that prompt commit
                            // its terminal cancellation first; normal dispatch then
                            // tears down the adapter and answers unload.
                            None
                        }
                        AcpRequest::AcpDeleteSessionRequest(delete)
                            if delete.session_id.as_deref().unwrap_or("main")
                                == user_session_id =>
                        {
                            let key = durable_route_key(ownership, &user_session_id);
                            self.cancel_pending_permissions(&key, "session_deleted");
                            self.signal_prompt_cancellation(&key);
                            // As with unload, queue deletion until the prompt has
                            // durably recorded its terminal cancellation.
                            None
                        }
                        _ => None,
                    }
                }
            };
        }
        None
    }
}

struct AcpHandlerOutput {
    response: Result<AcpResponse, SidecarError>,
    events: Vec<agentos_native_sidecar::wire::EventFrame>,
}

impl AcpHandlerOutput {
    fn response(response: Result<AcpResponse, SidecarError>) -> Self {
        Self {
            response,
            events: Vec::new(),
        }
    }
}

fn ownership_connection_id(ownership: &OwnershipScope) -> String {
    match ownership {
        OwnershipScope::ConnectionOwnership(inner) => inner.connection_id.clone(),
        OwnershipScope::SessionOwnership(inner) => inner.connection_id.clone(),
        OwnershipScope::VmOwnership(inner) => inner.connection_id.clone(),
    }
}

fn durable_route_key(ownership: &OwnershipScope, session_id: &str) -> String {
    let (scope, components): (&str, Vec<&str>) = match ownership {
        OwnershipScope::ConnectionOwnership(inner) => {
            ("connection", vec![inner.connection_id.as_str(), session_id])
        }
        OwnershipScope::SessionOwnership(inner) => (
            "session",
            vec![
                inner.connection_id.as_str(),
                inner.session_id.as_str(),
                session_id,
            ],
        ),
        OwnershipScope::VmOwnership(inner) => (
            "vm",
            vec![
                inner.connection_id.as_str(),
                inner.session_id.as_str(),
                inner.vm_id.as_str(),
                session_id,
            ],
        ),
    };
    let mut key = String::from(scope);
    for component in components {
        key.push(':');
        key.push_str(&component.len().to_string());
        key.push(':');
        key.push_str(component);
    }
    key
}

fn session_store_error(error: agentos_native_sidecar::vm_sqlite::VmSqliteError) -> SidecarError {
    match error {
        error @ (agentos_native_sidecar::vm_sqlite::VmSqliteError::ResultTooLarge { .. }
        | agentos_native_sidecar::vm_sqlite::VmSqliteError::HistoryEventBatchTooLarge {
            ..
        }
        | agentos_native_sidecar::vm_sqlite::VmSqliteError::HistoryByteBatchTooLarge {
            ..
        }
        | agentos_native_sidecar::vm_sqlite::VmSqliteError::DurableCollectionLimit {
            ..
        }) => SidecarError::InvalidState(error.to_string()),
        error => SidecarError::InvalidState(format!("session_storage_error: {error}")),
    }
}

fn validate_user_session_id(session_id: &str) -> Result<(), SidecarError> {
    if session_id.is_empty() || session_id.len() > 256 || session_id.as_bytes().contains(&0) {
        return Err(SidecarError::InvalidState(String::from(
            "invalid_session_id: sessionId must contain 1..=256 bytes without NUL",
        )));
    }
    Ok(())
}

fn default_session_id(session_id: Option<String>) -> Result<String, SidecarError> {
    let session_id = session_id.unwrap_or_else(|| String::from("main"));
    validate_user_session_id(&session_id)?;
    Ok(session_id)
}

fn safe_sequence(sequence: u64) -> Result<i64, SidecarError> {
    i64::try_from(sequence)
        .ok()
        .filter(|sequence| *sequence <= 9_007_199_254_740_991)
        .ok_or_else(|| {
            SidecarError::InvalidState(String::from(
                "invalid_history_cursor: sequence exceeds the JavaScript-safe integer range",
            ))
        })
}

fn parse_json_array(text: &str, field: &str) -> Result<Vec<Value>, SidecarError> {
    serde_json::from_str::<Vec<Value>>(text)
        .map_err(|error| SidecarError::InvalidState(format!("invalid {field} JSON array: {error}")))
}

fn parse_mcp_servers(text: &str) -> Result<Vec<McpServer>, SidecarError> {
    serde_json::from_str::<Vec<McpServer>>(text).map_err(|error| {
        SidecarError::InvalidState(format!(
            "invalid mcpServers: expected exact upstream ACP McpServer values: {error}"
        ))
    })
}

fn parse_string_map(text: &str, field: &str) -> Result<HashMap<String, String>, SidecarError> {
    serde_json::from_str::<HashMap<String, String>>(text).map_err(|error| {
        SidecarError::InvalidState(format!("invalid {field} JSON object: {error}"))
    })
}

fn canonical_creation_options(
    request: &AcpOpenSessionRequest,
    cwd: &str,
) -> Result<String, SidecarError> {
    let additional_directories = request
        .additional_directories
        .as_deref()
        .map(|value| parse_json_array(value, "additionalDirectories"))
        .transpose()?
        .unwrap_or_default();
    let env: BTreeMap<String, String> = request
        .env
        .as_deref()
        .map(|value| parse_string_map(value, "env"))
        .transpose()?
        .unwrap_or_default()
        .into_iter()
        .collect();
    let mcp_servers = request
        .mcp_servers
        .as_deref()
        .map(parse_mcp_servers)
        .transpose()?
        .unwrap_or_default();
    let permission_policy = request.permission_policy.as_deref().unwrap_or("allow_all");
    if !matches!(permission_policy, "reject_all" | "ask" | "allow_all") {
        return Err(SidecarError::InvalidState(format!(
            "invalid_permission_policy: {permission_policy}"
        )));
    }
    serde_json::to_string(&json!({
        "formatVersion": 1,
        "cwd": cwd,
        "additionalDirectories": additional_directories,
        "env": env,
        "mcpServers": mcp_servers,
        "permissionPolicy": permission_policy,
        "skipOsInstructions": request.skip_os_instructions.unwrap_or(false),
        "additionalInstructions": request.additional_instructions,
    }))
    .map_err(|error| SidecarError::InvalidState(error.to_string()))
}

async fn required_stored_session(
    store: &SessionStore,
    session_id: &str,
) -> Result<StoredSession, SidecarError> {
    store
        .get(session_id)
        .await
        .map_err(session_store_error)?
        .ok_or_else(|| SidecarError::InvalidState(format!("session_not_found: {session_id}")))
}

fn stored_session_response(
    session: StoredSession,
    constructor: impl FnOnce(AcpDurableSessionInfo) -> AcpResponse,
) -> Result<AcpResponse, SidecarError> {
    Ok(constructor(stored_session_info(session)?))
}

fn stored_session_info(session: StoredSession) -> Result<AcpDurableSessionInfo, SidecarError> {
    Ok(AcpDurableSessionInfo {
        session_id: session.session_id,
        agent: session.agent,
        cwd: session.cwd,
        additional_directories: session.additional_directories_json,
        state: session.state_json,
        latest_sequence: u64::try_from(session.latest_sequence).map_err(|_| {
            SidecarError::InvalidState(String::from("invalid stored latest sequence"))
        })?,
        title: session.title,
        metadata: session.metadata_json,
        created_at: timestamp(session.created_at_ms).map_err(session_store_error)?,
        updated_at: timestamp(session.updated_at_ms).map_err(session_store_error)?,
    })
}

fn stored_session_summary_info(
    session: StoredSessionSummary,
) -> Result<AcpDurableSessionInfo, SidecarError> {
    Ok(AcpDurableSessionInfo {
        session_id: session.session_id,
        agent: session.agent,
        cwd: session.cwd,
        additional_directories: session.additional_directories_json,
        state: session.state_json,
        latest_sequence: u64::try_from(session.latest_sequence).map_err(|_| {
            SidecarError::InvalidState(String::from("invalid stored latest sequence"))
        })?,
        title: session.title,
        metadata: session.metadata_json,
        created_at: timestamp(session.created_at_ms).map_err(session_store_error)?,
        updated_at: timestamp(session.updated_at_ms).map_err(session_store_error)?,
    })
}

fn encode_list_cursor(session: &StoredSessionSummary) -> Result<String, SidecarError> {
    let payload = serde_json::to_vec(&(session.updated_at_ms, &session.session_id))
        .map_err(|error| SidecarError::InvalidState(error.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload))
}

fn decode_list_cursor(cursor: &str) -> Result<(i64, String), SidecarError> {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| {
            SidecarError::InvalidState(String::from("invalid_session_cursor: malformed cursor"))
        })?;
    serde_json::from_slice(&payload).map_err(|_| {
        SidecarError::InvalidState(String::from("invalid_session_cursor: malformed cursor"))
    })
}

fn json_strings_to_array_text(values: &[String]) -> Result<String, SidecarError> {
    let values = values
        .iter()
        .map(|value| serde_json::from_str::<Value>(value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            SidecarError::InvalidState(format!("invalid ACP configuration option: {error}"))
        })?;
    serde_json::to_string(&values).map_err(|error| SidecarError::InvalidState(error.to_string()))
}

fn decode_request(payload: &[u8]) -> Result<AcpRequest, SidecarError> {
    serde_bare::from_slice(payload)
        .map_err(|error| SidecarError::InvalidState(format!("invalid ACP request: {error}")))
}

fn encode_response(response: AcpResponse) -> Result<Vec<u8>, SidecarError> {
    serde_bare::to_vec(&response)
        .map_err(|error| SidecarError::InvalidState(format!("invalid ACP response: {error}")))
}

fn encode_event(event: AcpEvent) -> Result<Vec<u8>, SidecarError> {
    serde_bare::to_vec(&event)
        .map_err(|error| SidecarError::InvalidState(format!("invalid ACP event: {error}")))
}

fn encode_callback(callback: AcpCallback) -> Result<Vec<u8>, SidecarError> {
    serde_bare::to_vec(&callback)
        .map_err(|error| SidecarError::InvalidState(format!("invalid ACP callback: {error}")))
}

fn error_response(error: SidecarError) -> AcpResponse {
    AcpResponse::AcpErrorResponse(AcpErrorResponse {
        code: error_code(&error),
        message: error.to_string(),
    })
}

fn error_code(error: &SidecarError) -> String {
    let code = match error {
        SidecarError::ResourceLimit(_) => "resource_limit",
        SidecarError::InvalidState(message) => message
            .split_once(':')
            .map(|(prefix, _)| prefix)
            .filter(|prefix| {
                !prefix.is_empty()
                    && prefix.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                    })
            })
            .unwrap_or("invalid_state"),
        SidecarError::ProtocolVersionMismatch(_) => "protocol_version_mismatch",
        SidecarError::BridgeVersionMismatch(_) => "bridge_version_mismatch",
        SidecarError::Conflict(_) => "conflict",
        SidecarError::Unauthorized(_) => "unauthorized",
        SidecarError::Unsupported(_) => "unsupported",
        SidecarError::FrameTooLarge(_) => "frame_too_large",
        SidecarError::Kernel(_) => "kernel",
        SidecarError::Plugin(_) => "plugin",
        SidecarError::Execution(_) => "execution",
        SidecarError::Bridge(_) => "bridge",
        SidecarError::Io(_) => "io",
    };
    String::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_protocol::PROTOCOL_VERSION;

    #[test]
    fn acp_extension_uses_agent_os_namespace() {
        assert_eq!(AcpExtension::new().namespace(), ACP_EXTENSION_NAMESPACE);
    }

    #[test]
    fn omitted_session_permission_policy_defaults_to_allow_all() {
        let request = AcpOpenSessionRequest {
            session_id: Some(String::from("main")),
            agent: String::from("pi"),
            cwd: None,
            additional_directories: None,
            env: None,
            mcp_servers: None,
            permission_policy: None,
            skip_os_instructions: None,
            additional_instructions: None,
        };

        let options: Value = serde_json::from_str(
            &canonical_creation_options(&request, "/home/agentos")
                .expect("omitted permission policy is valid"),
        )
        .expect("canonical creation options are JSON");
        assert_eq!(
            options.get("permissionPolicy").and_then(Value::as_str),
            Some("allow_all")
        );
    }

    #[test]
    fn permission_public_identity_is_unique_and_hides_acp_session_ids() {
        let native = json!({
            "sessionId": "native-session",
            "toolCall": { "toolCallId": "tool-1", "title": "write" },
            "options": [{ "optionId": "yes", "name": "Yes", "kind": "allow_once" }],
            "_meta": { "adapter": "preserved" },
        });
        let (first_id, first_json) =
            public_permission_request(&native, "public-session").expect("first request");
        let (second_id, _) =
            public_permission_request(&native, "public-session").expect("second request");
        assert_ne!(first_id, second_id);
        assert!(!first_id.contains("native-session"));
        let public: Value = serde_json::from_str(&first_json).expect("public request JSON");
        assert_eq!(
            public.get("sessionId").and_then(Value::as_str),
            Some("public-session")
        );
        assert_eq!(public.get("toolCall"), native.get("toolCall"));
        assert_eq!(public.get("options"), native.get("options"));
        assert_eq!(public.get("_meta"), native.get("_meta"));
        assert!(!first_json.contains("native-session"));
    }

    #[test]
    fn automatic_permission_policy_prefers_one_shot_regardless_of_adapter_order() {
        let params = json!({
            "options": [
                { "optionId": "always", "kind": "allow_always" },
                { "optionId": "once", "kind": "allow_once" },
                { "optionId": "reject-always", "kind": "reject_always" },
                { "optionId": "reject-once", "kind": "reject_once" }
            ]
        });
        assert_eq!(
            permission_option_for_kinds(&params, &["allow_once", "allow_always"]),
            Some(String::from("once"))
        );
        assert_eq!(
            permission_option_for_kinds(&params, &["reject_once", "reject_always"]),
            Some(String::from("reject-once"))
        );
        let unsatisfied = automatic_permission_option("reject_all", &json!({ "options": [] }))
            .expect_err("reject_all requires a native rejection option");
        assert_eq!(error_code(&unsatisfied), "permission_policy_unsatisfied");
    }

    #[test]
    fn terminal_permission_responses_preserve_specific_public_reasons() {
        for (stored, expected) in [
            ("accepted", "already_resolved"),
            ("prompt_cancelled", "prompt_cancelled"),
            ("adapter_exited", "adapter_exited"),
            ("session_deleted", "session_deleted"),
            ("vm_shutdown", "vm_shutdown"),
        ] {
            let response = permission_terminal_response(PendingRequestResolution::Terminal {
                reason: stored.to_owned(),
                event: None,
            });
            assert_eq!(response.status, "not_pending");
            assert_eq!(response.reason.as_deref(), Some(expected));
        }
        let missing = permission_terminal_response(PendingRequestResolution::NotFound);
        assert_eq!(missing.status, "not_pending");
        assert_eq!(missing.reason.as_deref(), Some("request_not_found"));
    }

    #[test]
    fn obsolete_permission_protocol_and_backend_surfaces_stay_removed() {
        let backend_sources = [
            include_str!("mod.rs"),
            include_str!("turn.rs"),
            include_str!("runtime.rs"),
            include_str!("restore.rs"),
            include_str!("../session_store.rs"),
            include_str!("../../../agentos-protocol/protocol/agent_os_acp_v1.bare"),
        ];
        for removed in [
            concat!("AcpPermission", "Callback"),
            concat!("AcpPermission", "RequestEvent"),
            concat!("permission_", "result"),
            concat!("expires_at", "_ms"),
            concat!("expires", "At"),
            concat!("permission_", "timeout"),
        ] {
            assert!(
                backend_sources
                    .iter()
                    .all(|source| !source.contains(removed)),
                "obsolete permission token returned: {removed}"
            );
        }

        let rust_client_sources = [
            include_str!("../../../client/src/session.rs"),
            include_str!("../../../client/src/agent_os.rs"),
            include_str!("../../../client/src/lib.rs"),
        ];
        for removed in [
            concat!("pub struct Durable", "PermissionRequest"),
            concat!("on_permission", "_request"),
            concat!("AcpPermission", "Callback"),
        ] {
            assert!(
                rust_client_sources
                    .iter()
                    .all(|source| !source.contains(removed)),
                "obsolete Rust permission surface returned: {removed}"
            );
        }
    }

    #[test]
    fn configured_acp_limit_errors_preserve_stable_wire_codes() {
        let mut limits = AcpLimits {
            max_prompt_bytes: 3,
            ..AcpLimits::default()
        };
        let bytes_error = parse_content_blocks("[{}]", "main", &limits)
            .expect_err("prompt bytes must be bounded");
        assert_eq!(error_code(&bytes_error), "acp_prompt_bytes_limit");

        limits.max_prompt_bytes = 1024;
        limits.max_prompt_blocks = 1;
        let blocks_error = parse_content_blocks("[{},{}]", "main", &limits)
            .expect_err("prompt blocks must be bounded");
        assert_eq!(error_code(&blocks_error), "acp_prompt_blocks_limit");

        assert_eq!(
            error_code(&SidecarError::InvalidState(String::from(
                "acp_prompt_bytes_limit: raise limits.acp.maxPromptBytes"
            ))),
            "acp_prompt_bytes_limit"
        );
        assert_eq!(
            error_code(&SidecarError::InvalidState(String::from(
                "acp_prompt_blocks_limit: raise limits.acp.maxPromptBlocks"
            ))),
            "acp_prompt_blocks_limit"
        );
        assert_eq!(
            error_code(&session_store_error(
                agentos_native_sidecar::vm_sqlite::VmSqliteError::HistoryByteBatchTooLarge {
                    used: 2,
                    limit: 1,
                }
            )),
            "acp_history_bytes_limit"
        );
        assert_eq!(
            error_code(&session_store_error(
                agentos_native_sidecar::vm_sqlite::VmSqliteError::ResultTooLarge {
                    used: 2,
                    limit: 1,
                }
            )),
            "sqlite_result_limit"
        );
    }

    #[test]
    fn adapter_gone_classifier_matches_both_observation_paths() {
        // In-pump observation: the exchange loop saw the ProcessExitedEvent.
        let exited = SidecarError::InvalidState(format!(
            "ACP adapter process acp-agent-3 {ADAPTER_EXITED_ERROR_MARKER} 7 before response id=4"
        ));
        assert!(is_adapter_gone_error(&exited));
        assert_eq!(adapter_exit_code_from_error(&exited), Some(7));

        // Lazy observation: a request write to an already-reaped adapter fails
        // with agentos's process-table error (the exact production shape:
        // "VM vm-5 has no active process agent-6"). No exit code is observed.
        let gone =
            SidecarError::InvalidState(String::from("VM vm-5 has no active process agent-6"));
        assert!(is_adapter_gone_error(&gone));
        assert_eq!(adapter_exit_code_from_error(&gone), None);

        // Transient failures must NOT classify as adapter-gone, or the session
        // would be restarted/evicted on retryable errors.
        let transient = SidecarError::InvalidState(String::from(
            "timed out waiting for ACP response id=4; sent session/cancel notification",
        ));
        assert!(!is_adapter_gone_error(&transient));
        assert_eq!(adapter_exit_code_from_error(&transient), None);
    }

    #[test]
    fn unknown_session_normalization_pins_known_adapter_shape() {
        let mut adapter_response = serde_json::json!({
            "error": { "code": -32603, "message": "Internal error", "data": { "details": "NotFoundError" } }
        });
        normalize_unknown_session_error(&mut adapter_response);
        assert_eq!(
            adapter_response
                .pointer("/error/data/kind")
                .and_then(Value::as_str),
            Some("unknown_session")
        );
        assert!(is_unknown_session_error(&adapter_response));

        let mut malformed = serde_json::json!({
            "error": { "code": -32602, "message": "Invalid params",
                       "data": { "_errors": [], "sessionId": { "_errors": ["expected string"] } } }
        });
        normalize_unknown_session_error(&mut malformed);
        assert!(!is_unknown_session_error(&malformed));

        let mut other_internal = serde_json::json!({
            "error": { "code": -32603, "message": "Internal error", "data": { "details": "SomethingElse" } }
        });
        normalize_unknown_session_error(&mut other_internal);
        assert!(!is_unknown_session_error(&other_internal));
    }

    #[test]
    fn unknown_session_matcher_recognizes_normalized_sentinel_only() {
        assert!(is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32000, "message": "x", "data": { "kind": "unknown_session" } }
        })));

        // Raw OpenCode shape must be normalized before matching.
        assert!(!is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32603, "message": "Internal error", "data": { "details": "NotFoundError" } }
        })));
        assert!(!is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32602, "message": "Invalid params",
                       "data": { "_errors": [], "sessionId": { "_errors": ["expected string"] } } }
        })));
        // Must NOT match: a -32603 internal error that is NOT a NotFoundError.
        assert!(!is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32603, "message": "Internal error", "data": { "details": "SomethingElse" } }
        })));
        // Must NOT match: NotFoundError under a non--32603 code (different failure).
        assert!(!is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32000, "data": { "details": "NotFoundError" } }
        })));
        // Must NOT match: a successful response or a bare transport error.
        assert!(!is_unknown_session_error(
            &serde_json::json!({ "result": {} })
        ));
        assert!(!is_unknown_session_error(&serde_json::json!({
            "error": { "code": -32603, "message": "Internal error" }
        })));
    }

    #[test]
    fn initialize_protocol_version_is_validated() {
        let result = Map::from_iter([(
            String::from("protocolVersion"),
            Value::Number(i64::from(PROTOCOL_VERSION).into()),
        )]);

        validate_initialize_result(&result, i32::from(PROTOCOL_VERSION))
            .expect("matching protocol version");
    }

    #[test]
    fn bounded_stdout_lines_preserve_partial_then_emit() {
        let mut buffer = String::new();
        let lines = append_stdout_chunk(&mut buffer, br#"{"a":"#, 8).expect("partial chunk");
        assert!(lines.is_empty());
        assert_eq!(buffer, r#"{"a":"#);

        let lines = append_stdout_chunk(&mut buffer, b"1}\n", 8).expect("complete line");
        assert_eq!(lines, vec![r#"{"a":1}"#]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn bounded_stdout_lines_reject_complete_overlong_line() {
        let mut buffer = String::new();
        let error =
            append_stdout_chunk(&mut buffer, b"123456789\n", 8).expect_err("line exceeds cap");
        assert!(error
            .to_string()
            .contains("ACP adapter emitted a line longer than 8 bytes"));
    }

    #[test]
    fn bounded_stdout_lines_reject_unterminated_overlong_line() {
        let mut buffer = String::new();
        let error =
            append_stdout_chunk(&mut buffer, b"123456789", 8).expect_err("line exceeds cap");
        assert!(error
            .to_string()
            .contains("ACP adapter emitted a line longer than 8 bytes"));
    }

    #[test]
    fn session_cancel_notification_has_acp_shape() {
        assert_eq!(
            session_cancel_notification("adapter-session"),
            json!({
                "jsonrpc": "2.0",
                "method": "session/cancel",
                "params": {
                    "sessionId": "adapter-session",
                },
            })
        );
    }

    #[test]
    fn cancel_method_not_found_detection_accepts_error_data_or_message() {
        assert!(is_cancel_method_not_found(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": {
                "code": -32601,
                "message": "method not found",
                "data": { "method": "session/cancel" },
            },
        })));
        assert!(is_cancel_method_not_found(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": {
                "code": -32601,
                "message": "unknown method session/cancel",
            },
        })));
        assert!(!is_cancel_method_not_found(&json!({
            "jsonrpc": "2.0",
            "id": 4,
            "error": {
                "code": -32000,
                "message": "session/cancel failed",
            },
        })));
    }

    #[test]
    fn cancel_fallback_response_matches_adapter_shape() {
        assert_eq!(
            cancel_notification_fallback_response(Value::Number(4.into())),
            json!({
                "jsonrpc": "2.0",
                "id": 4,
                "result": {
                    "cancelled": false,
                    "requested": true,
                    "via": "notification-fallback",
                },
            })
        );
    }

    #[test]
    fn request_timeout_uses_acp_method_overrides() {
        assert_eq!(request_timeout("initialize"), Some(Duration::from_secs(60)));
        assert_eq!(
            request_timeout("session/new"),
            Some(Duration::from_secs(60))
        );
        assert_eq!(request_timeout("session/prompt"), None);
        assert_eq!(SESSION_CLOSE_TIMEOUT, Duration::from_secs(5));
        assert_eq!(ACP_MACHINE_HOST_CALLBACK_TIMEOUT, Duration::from_secs(120));
        assert_eq!(
            request_timeout("session/set_mode"),
            Some(Duration::from_secs(120))
        );
    }

    #[test]
    fn active_acp_progress_resets_inactivity_warnings() {
        let start = Instant::now();
        let mut warnings = InactivityWarnings::new_at("sent prompt", start);

        for step in 1..=4 {
            let activity_at = start + Duration::from_secs(step * 29);
            assert_eq!(warnings.take_due(activity_at), None);
            warnings.record_at(format!("streamed progress {step}"), activity_at);
        }

        assert_eq!(
            warnings.take_due(start + Duration::from_secs(4 * 29 + 29)),
            None
        );
    }

    #[test]
    fn inactive_prompt_and_permission_waits_warn_repeatedly_without_a_deadline() {
        let start = Instant::now();
        let mut prompt = InactivityWarnings::new_at("sent prompt", start);
        let first = prompt
            .take_due(start + ACP_INACTIVITY_WARNING_INTERVAL)
            .expect("first prompt inactivity warning");
        assert_eq!(first.elapsed, Duration::from_secs(30));
        assert_eq!(first.inactive, Duration::from_secs(30));
        assert_eq!(first.last_activity, "sent prompt");
        assert!(prompt
            .take_due(start + ACP_INACTIVITY_WARNING_INTERVAL * 2)
            .is_some());

        let mut permission = InactivityWarnings::new_at("emitted permission request per_1", start);
        assert!(permission
            .take_due(start + ACP_INACTIVITY_WARNING_INTERVAL)
            .is_some());
        assert!(permission
            .take_due(start + ACP_INACTIVITY_WARNING_INTERVAL * 2)
            .is_some());

        assert_eq!(request_timeout("session/prompt"), None);
    }

    #[test]
    fn model_config_option_detection_accepts_id_or_category() {
        assert!(is_model_config_option(&json!({
            "id": "model",
            "category": "provider",
        })));
        assert!(is_model_config_option(&json!({
            "id": "provider-model",
            "category": "model",
        })));
        assert!(!is_model_config_option(&json!({
            "id": "thought-level",
            "category": "thought_level",
        })));
    }

    #[test]
    fn session_new_session_id_falls_back_to_wrapper_id() {
        assert_eq!(
            session_id_from_session_result(
                &Map::from_iter([(String::from("sessionId"), json!("adapter-session"))]),
                "acp-agent-1",
            ),
            "adapter-session"
        );
        assert_eq!(
            session_id_from_session_result(&Map::new(), "acp-agent-1"),
            "acp-agent-1"
        );
        assert_eq!(
            session_id_from_session_result(
                &Map::from_iter([(String::from("sessionId"), json!(""))]),
                "acp-agent-1",
            ),
            "acp-agent-1"
        );
    }

    #[test]
    fn timeout_error_response_includes_structured_diagnostics() {
        let response = timeout_error_response(
            7,
            "session/prompt",
            Duration::from_secs(120),
            "acp-agent-1",
            "sent session/cancel notification",
            vec![
                String::from("sent request session/prompt id=7"),
                String::from("received notification session/update"),
            ],
        );

        assert_eq!(response["jsonrpc"], json!("2.0"));
        assert_eq!(response["id"], json!(7));
        assert_eq!(response["error"]["code"], json!(-32000));
        assert!(response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("ACP request session/prompt (id=7) timed out after 120000ms"));
        assert_eq!(response["error"]["data"]["kind"], json!("acp_timeout"));
        assert_eq!(response["error"]["data"]["method"], json!("session/prompt"));
        assert_eq!(response["error"]["data"]["id"], json!(7));
        assert_eq!(response["error"]["data"]["timeoutMs"], json!(120000));
        assert_eq!(
            response["error"]["data"]["transportState"],
            json!("sent session/cancel notification")
        );
        assert_eq!(
            response["error"]["data"]["recentActivity"],
            json!([
                "sent request session/prompt id=7",
                "received notification session/update"
            ])
        );
    }

    #[test]
    fn durable_route_keys_preserve_full_vm_ownership_identity() {
        use agentos_native_sidecar::wire::VmOwnership;

        let segmented_one = OwnershipScope::VmOwnership(VmOwnership {
            connection_id: String::from("connection:session"),
            session_id: String::from("owner"),
            vm_id: String::from("vm"),
        });
        let segmented_two = OwnershipScope::VmOwnership(VmOwnership {
            connection_id: String::from("connection"),
            session_id: String::from("session:owner"),
            vm_id: String::from("vm"),
        });
        assert_ne!(
            durable_route_key(&segmented_one, "main"),
            durable_route_key(&segmented_two, "main"),
            "delimiter-containing ownership fields must not alias"
        );

        let other_vm = OwnershipScope::VmOwnership(VmOwnership {
            connection_id: String::from("connection:session"),
            session_id: String::from("owner"),
            vm_id: String::from("other-vm"),
        });
        assert_ne!(
            durable_route_key(&segmented_one, "main"),
            durable_route_key(&other_vm, "main")
        );
        assert_ne!(
            durable_route_key(&segmented_one, "main"),
            durable_route_key(&segmented_one, "other-session")
        );
    }

    /// Drive a future that only awaits uncontended in-memory state (e.g. a free
    /// `tokio::sync::Mutex`) to completion without a runtime: such a future is
    /// `Ready` on its first poll. Panics if it parks, which would mean it touched
    /// real async I/O the unit test cannot service. Lets the sync test harness
    /// exercise the real async `Extension::on_dispose` wiring.
    fn poll_uncontended<F: std::future::Future>(future: F) -> F::Output {
        use std::task::{Context, Poll};
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => output,
            Poll::Pending => {
                panic!("future parked; expected uncontended in-memory completion")
            }
        }
    }

    fn test_session_record(session_id: &str, owner_connection_id: &str) -> LiveAcpRuntime {
        LiveAcpRuntime {
            acp_session_id: session_id.to_string(),
            user_session_id: None,
            owner_connection_id: owner_connection_id.to_string(),
            agent_type: String::from("pi"),
            process_id: format!("acp-agent-{session_id}"),
            pid: None,
            modes: None,
            config_options: Vec::new(),
            agent_capabilities: None,
            agent_info: None,
            stdout_buffer: String::new(),
            next_request_id: 3,
            closed: false,
            pending_preamble: None,
        }
    }

    #[test]
    fn connection_teardown_evicts_only_that_connections_sessions() {
        // Regression: sessions were removed ONLY by the explicit stop_acp_runtime
        // RPC, so a connection that disconnected without closing its sessions
        // leaked every record (incl. its stdout_buffer) forever. The
        // connection-teardown path must drop exactly that connection's sessions.
        let ext = AcpExtension::new();
        {
            let mut sessions = ext.sessions.try_lock().expect("uncontended sessions lock");
            sessions.insert(String::from("s1"), test_session_record("s1", "conn-a"));
            sessions.insert(String::from("s2"), test_session_record("s2", "conn-a"));
            sessions.insert(String::from("s3"), test_session_record("s3", "conn-b"));
        }

        let reaped = {
            let mut sessions = ext.sessions.try_lock().expect("uncontended sessions lock");
            evict_sessions_for_connection(&mut sessions, "conn-a")
        };

        assert_eq!(reaped.len(), 2, "both conn-a adapter processes reaped");
        let sessions = ext.sessions.try_lock().expect("uncontended sessions lock");
        assert!(!sessions.contains_key("s1"), "conn-a session evicted");
        assert!(!sessions.contains_key("s2"), "conn-a session evicted");
        assert!(
            sessions.contains_key("s3"),
            "other connection's session must survive its peer's teardown"
        );
    }

    #[test]
    fn on_dispose_clears_every_session_record() {
        // H4 (the actually-wired ACP-session leak fix): on extension/sidecar
        // teardown `Extension::on_dispose` must drop EVERY remaining session
        // record so no `stdout_buffer` survives the host process — not just the
        // records for one connection.
        let ext = AcpExtension::new();
        {
            let mut sessions = ext.sessions.try_lock().expect("uncontended sessions lock");
            sessions.insert(String::from("s1"), test_session_record("s1", "conn-a"));
            sessions.insert(String::from("s2"), test_session_record("s2", "conn-b"));
            sessions.insert(String::from("s3"), test_session_record("s3", "conn-c"));
        }

        // Drive the real wired async `on_dispose` impl; it only awaits the
        // uncontended `sessions` mutex, so it completes on the first poll.
        poll_uncontended(ext.on_dispose()).expect("on_dispose succeeds");

        let sessions = ext.sessions.try_lock().expect("uncontended sessions lock");
        assert!(
            sessions.is_empty(),
            "on_dispose must clear the entire sessions map"
        );
    }

    #[test]
    fn capped_stdout_buffer_never_exceeds_limit() {
        let limit = DEFAULT_ACP_MAX_READ_LINE_BYTES;
        let mut buffer = "x".repeat(limit + 4096);
        cap_stdout_buffer(&mut buffer, limit);
        assert!(
            buffer.len() <= limit,
            "retained stdout_buffer must be bounded"
        );

        // A buffer already within the cap is left untouched.
        let mut small = String::from("partial-line");
        cap_stdout_buffer(&mut small, limit);
        assert_eq!(small, "partial-line");
    }

    #[test]
    fn capped_stdout_buffer_truncates_on_utf8_char_boundary() {
        // All-ASCII inputs never exercise the `is_char_boundary` adjustment loop.
        // A buffer of multi-byte chars forces the naive split point off a char
        // boundary, so the loop must advance it; the result must stay valid UTF-8
        // (no panic / no split char) and keep the most recent trailing bytes.
        const CHAR: char = '€'; // 3 bytes in UTF-8
        let limit = DEFAULT_ACP_MAX_READ_LINE_BYTES;
        let original = CHAR.to_string().repeat(limit); // 3 * limit bytes, far over the cap
        let mut buffer = original.clone();
        cap_stdout_buffer(&mut buffer, limit);

        assert!(
            buffer.len() <= limit,
            "capped multi-byte buffer must be bounded"
        );
        // No char was split: a homogeneous 3-byte-char buffer can only have a
        // length that is a multiple of 3 if every retained char is intact.
        assert_eq!(
            buffer.len() % CHAR.len_utf8(),
            0,
            "cap must truncate on a UTF-8 char boundary, not mid-char"
        );
        assert!(
            std::str::from_utf8(buffer.as_bytes()).is_ok(),
            "capped buffer must remain valid UTF-8"
        );
        assert!(
            buffer.chars().all(|c| c == CHAR),
            "every retained char survived intact"
        );
        // The trailing (most recent) bytes are kept, not the head.
        assert!(
            !buffer.is_empty() && original.ends_with(&buffer),
            "cap keeps the trailing partial-line bytes"
        );
    }

    #[test]
    fn adapter_exit_error_is_recognized_for_eviction() {
        // Build the EXACT error string the `ProcessExitedEvent` arm of
        // `session_request` emits (it embeds `ADAPTER_EXITED_ERROR_MARKER`
        // directly), so a change to the producer's wording that drops the marker
        // would break this test instead of silently disabling session eviction.
        let process_id = "acp-agent-1";
        let exit_code = 1;
        let response_id = 3;
        let exited = SidecarError::InvalidState(format!(
            "ACP adapter process {process_id} {ADAPTER_EXITED_ERROR_MARKER} {exit_code} before response id={response_id}",
        ));
        assert!(
            is_adapter_exited_error(&exited),
            "the real adapter-exit error must trigger session eviction"
        );

        // Transient failures must NOT be treated as adapter exit (would evict a
        // session that is still alive).
        let timed_out =
            SidecarError::InvalidState(String::from("timed out waiting for ACP response id=3"));
        assert!(!is_adapter_exited_error(&timed_out));
        let broken_pipe = SidecarError::InvalidState(String::from(
            "failed to write ACP request to adapter stdin: broken pipe",
        ));
        assert!(!is_adapter_exited_error(&broken_pipe));
    }
}
