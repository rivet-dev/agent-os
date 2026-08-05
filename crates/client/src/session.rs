//! Agent sessions (ACP) methods + supporting types.
//!
//! Mirrors the durable session surface in `packages/core/src/session-api.ts`.
//! Agent types are resolved dynamically from projected package manifests; the
//! client has no hardcoded agent registry.
//!
//! The sidecar owns ACP JSON-RPC, adapter restoration, and SQLite history. This
//! client exposes typed durable operations and exact upstream ACP data types,
//! not a second raw JSON-RPC session API.

use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;

use anyhow::Result;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use agent_client_protocol_schema::v1::McpServer as McpServerConfig;

pub use agent_client_protocol_schema::v1::{
    AvailableCommandsUpdate, ConfigOptionUpdate, ContentBlock, ContentChunk, CurrentModeUpdate,
    Meta, PermissionOption, Plan, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SessionConfigOption, SessionInfoUpdate, SessionUpdate, StopReason,
    ToolCall, ToolCallUpdate, UsageUpdate,
};

use agentos_acp_protocol::generated::v1::{
    AcpCancelPromptRequest, AcpDeleteSessionRequest, AcpDurableEvent, AcpDurableHistoryEntry,
    AcpDurableSessionInfo, AcpGetDurableSessionRequest, AcpGetSessionAgentInfoRequest,
    AcpGetSessionCapabilitiesRequest, AcpGetSessionConfigRequest, AcpListAgentsRequest,
    AcpListDurableSessionsRequest, AcpOpenSessionRequest, AcpPromptRequest, AcpReadHistoryRequest,
    AcpRequest, AcpRespondPermissionRequest, AcpResponse, AcpSetSessionConfigOptionRequest,
    AcpUnloadSessionRequest,
};
use agentos_acp_protocol::ACP_EXTENSION_NAMESPACE;
use agentos_sidecar_client::wire;

use crate::agent_os::AgentOs;
use crate::config::Bindings;
use crate::error::ClientError;
use crate::stream::Subscription;
pub type DurableSessionEventStream = Pin<
    Box<
        dyn Stream<Item = std::result::Result<SessionStreamEntry, SessionSubscriptionError>> + Send,
    >,
>;
pub type DurableSessionEventSubscription = (DurableSessionEventStream, Subscription);
pub type AgentExitStream = Pin<Box<dyn Stream<Item = AgentExitEvent> + Send>>;
pub type AgentExitSubscription = (AgentExitStream, Subscription);

/// An unexpected ACP adapter process exit — a crash from the host's
/// perspective (any spontaneous exit without `unload_session`, including exit
/// code 0). Mirrors the wire
/// `AcpAgentExitedEvent` and the TS `AgentExitEvent`.
///
/// `restart` is always `"not_attempted"`: AgentOS evicts the live route and
/// never respawns the adapter or replays the interrupted request implicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRestartOutcome {
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExitEvent {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "agentType")]
    pub agent_type: String,
    #[serde(rename = "processId")]
    pub process_id: String,
    /// Host pid when reported by the transport; `None` when unavailable.
    pub pid: Option<u32>,
    /// Adapter exit code; `None` when the exit was observed indirectly.
    #[serde(rename = "exitCode")]
    pub exit_code: Option<i32>,
    pub restart: AgentRestartOutcome,
    #[serde(rename = "restartCount")]
    pub restart_count: u32,
    #[serde(rename = "maxRestarts")]
    pub max_restarts: u32,
}

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// A registry agent entry from `list_agents`. Mirrors the TS `AgentRegistryEntry`.
/// The client is npm-agnostic and parses no manifests: `list_agents` is a sidecar
/// ACP RPC that enumerates the projected `/opt/agentos` packages. The entry is just
/// the agent `id`; `installed` is always `true` (the package is materialized into
/// the VM at boot).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRegistryEntry {
    pub id: String,
    pub installed: bool,
}

/// Immutable options used to open a durable AgentOS session. Omitted fields are
/// forwarded as omissions so the sidecar remains the single owner of defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSessionInput {
    /// Caller-owned public session ID. Omission selects the documented `main`
    /// ID; AgentOS never allocates or returns a different public ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<McpServerConfig>>,
    /// Immutable AgentOS strategy for native ACP permission requests. Omission
    /// selects `AllowAll`; this does not configure VM permissions or tool access.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_policy: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_os_instructions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    RejectAll,
    Ask,
    AllowAll,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Running {
        #[serde(rename = "startedAt")]
        started_at: String,
    },
    Waiting {
        #[serde(rename = "waitingSince")]
        waiting_since: String,
        requests: Vec<PendingPermissionRequest>,
    },
    Failed {
        error: Value,
    },
}

/// SQLite-backed session metadata. Reading it never starts an ACP adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub agent: String,
    pub cwd: String,
    pub additional_directories: Vec<String>,
    pub state: SessionState,
    pub latest_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, Value>>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListSessionsInput {
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub sessions: Vec<SessionInfo>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptInput {
    pub session_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub session_id: String,
    pub message: Option<AgentMessage>,
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelPromptStatus {
    Cancelled,
    NoActivePrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionResponseStatus {
    Accepted,
    NotPending(PermissionTerminalReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTerminalReason {
    AlreadyResolved,
    PromptCancelled,
    AdapterExited,
    SessionDeleted,
    VmShutdown,
    RequestNotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionEventStatus {
    Accepted,
    NotPending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DurableSessionEvent {
    UserMessageChunk(ContentChunk),
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
    ToolCall(ToolCall),
    ToolCallUpdate(ToolCallUpdate),
    Plan(Plan),
    AvailableCommandsUpdate(AvailableCommandsUpdate),
    CurrentModeUpdate(CurrentModeUpdate),
    ConfigOptionUpdate(ConfigOptionUpdate),
    SessionInfoUpdate(SessionInfoUpdate),
    UsageUpdate(UsageUpdate),
    PermissionRequest {
        #[serde(rename = "requestId")]
        request_id: String,
        options: Vec<PermissionOption>,
        #[serde(rename = "toolCall")]
        tool_call: ToolCallUpdate,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
        meta: Option<Meta>,
    },
    PermissionResponse {
        #[serde(rename = "requestId")]
        request_id: String,
        outcome: RequestPermissionOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
        meta: Option<Meta>,
        status: PermissionEventStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<PermissionTerminalReason>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EphemeralSessionEvent {
    AgentMessageChunk(ContentChunk),
    AgentThoughtChunk(ContentChunk),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPermissionRequest {
    pub request_id: String,
    pub options: Vec<PermissionOption>,
    pub tool_call: ToolCallUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "_meta")]
    pub meta: Option<Meta>,
}

/// A bounded live session subscription fell behind. Durable updates remain in
/// SQLite and can be recovered with `read_history`; ephemeral deltas cannot be
/// recovered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionSubscriptionError {
    #[error("session subscription lagged by {skipped} entries; resume durable history with read_history")]
    Lagged { skipped: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableSessionEventEntry {
    pub durability: DurableEventKind,
    pub session_id: String,
    pub sequence: u64,
    pub timestamp: String,
    #[serde(flatten)]
    pub event: DurableSessionEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableEventKind {
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EphemeralEventKind {
    Ephemeral,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EphemeralSessionEventEntry {
    pub durability: EphemeralEventKind,
    pub session_id: String,
    pub after_sequence: u64,
    #[serde(flatten)]
    pub event: EphemeralSessionEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionStreamEntry {
    Durable(DurableSessionEventEntry),
    Ephemeral(EphemeralSessionEventEntry),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadHistoryInput {
    pub session_id: Option<String>,
    pub before: Option<u64>,
    pub after: Option<u64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistoryPage {
    pub events: Vec<DurableSessionEventEntry>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub revision: u64,
    pub options: Vec<SessionConfigOption>,
}

/// Native ACP configuration values are deliberately limited to the protocol's
/// string and boolean variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionConfigValue {
    String(String),
    Boolean(bool),
}

impl From<String> for SessionConfigValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<&str> for SessionConfigValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<bool> for SessionConfigValue {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    pub protocol_version: u64,
    pub load_session: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", flatten)]
    pub extensions: BTreeMap<String, Value>,
}

fn encode_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(Into::into)
}

fn encode_optional_json<T: Serialize>(value: Option<T>) -> Result<Option<String>> {
    value.as_ref().map(encode_json).transpose()
}

fn decode_json<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(Into::into)
}

fn decode_optional_json<T: for<'de> Deserialize<'de>>(value: Option<String>) -> Result<Option<T>> {
    value.as_deref().map(decode_json).transpose()
}

fn permission_policy_wire(policy: PermissionPolicy) -> String {
    match policy {
        PermissionPolicy::RejectAll => String::from("reject_all"),
        PermissionPolicy::Ask => String::from("ask"),
        PermissionPolicy::AllowAll => String::from("allow_all"),
    }
}

fn session_stream_id(entry: &SessionStreamEntry) -> &str {
    match entry {
        SessionStreamEntry::Durable(entry) => &entry.session_id,
        SessionStreamEntry::Ephemeral(entry) => &entry.session_id,
    }
}

fn decode_session_info(value: AcpDurableSessionInfo) -> Result<SessionInfo> {
    Ok(SessionInfo {
        session_id: value.session_id,
        agent: value.agent,
        cwd: value.cwd,
        additional_directories: decode_json(&value.additional_directories)?,
        state: decode_json(&value.state)?,
        latest_sequence: value.latest_sequence,
        title: value.title,
        metadata: decode_optional_json(value.metadata)?,
        created_at: value.created_at,
        updated_at: value.updated_at,
    })
}

fn decode_history_entry(value: AcpDurableHistoryEntry) -> Result<DurableSessionEventEntry> {
    Ok(DurableSessionEventEntry {
        durability: DurableEventKind::Durable,
        session_id: value.session_id,
        sequence: value.sequence,
        timestamp: value.timestamp,
        event: decode_durable_event(value.event)?,
    })
}

pub(crate) fn decode_durable_event(value: AcpDurableEvent) -> Result<DurableSessionEvent> {
    match value {
        AcpDurableEvent::AcpDurableSessionUpdate(event) => {
            let update: SessionUpdate = decode_json(&event.update)?;
            let mut value = serde_json::to_value(update)?;
            let object = value
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("ACP session update must serialize as an object"))?;
            let update_type = object
                .remove("sessionUpdate")
                .ok_or_else(|| anyhow::anyhow!("ACP session update is missing sessionUpdate"))?;
            object.insert(String::from("type"), update_type);
            serde_json::from_value(value).map_err(Into::into)
        }
        AcpDurableEvent::AcpDurablePermissionRequest(event) => {
            let request: RequestPermissionRequest = decode_json(&event.request)?;
            Ok(DurableSessionEvent::PermissionRequest {
                request_id: event.request_id,
                options: request.options,
                tool_call: request.tool_call,
                meta: request.meta,
            })
        }
        AcpDurableEvent::AcpDurablePermissionResponse(event) => {
            let response: RequestPermissionResponse = decode_json(&event.response)?;
            let status = match event.status.as_str() {
                "accepted" => PermissionEventStatus::Accepted,
                "not_pending" => PermissionEventStatus::NotPending,
                status => anyhow::bail!("invalid permission response event status: {status}"),
            };
            let reason = event
                .reason
                .as_deref()
                .map(permission_terminal_reason)
                .transpose()?;
            Ok(DurableSessionEvent::PermissionResponse {
                request_id: event.request_id,
                outcome: response.outcome,
                meta: response.meta,
                status,
                reason,
            })
        }
    }
}

fn decode_session_config_response(response: AcpResponse, operation: &str) -> Result<SessionConfig> {
    match response {
        AcpResponse::AcpSessionConfigResponse(response) => Ok(SessionConfig {
            revision: response.revision,
            options: decode_json(&response.options)?,
        }),
        AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
        other => Err(unexpected_acp_response(operation, other).into()),
    }
}

fn normalize_session_capabilities(value: Value) -> Result<SessionCapabilities> {
    let object = value.as_object().ok_or_else(|| {
        ClientError::Sidecar(String::from("malformed ACP agentCapabilities JSON"))
    })?;
    let prompt = object
        .get("promptCapabilities")
        .filter(|value| value.is_object())
        .cloned();
    let mcp = object
        .get("mcpCapabilities")
        .filter(|value| value.is_object())
        .cloned();
    let session = object
        .get("sessionCapabilities")
        .filter(|value| value.is_object())
        .cloned();
    let extensions = object
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "loadSession" | "promptCapabilities" | "mcpCapabilities" | "sessionCapabilities"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok(SessionCapabilities {
        protocol_version: crate::ACP_PROTOCOL_VERSION,
        load_session: object
            .get("loadSession")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        prompt,
        mcp,
        session,
        extensions,
    })
}

fn acp_operation_error(
    error: agentos_acp_protocol::generated::v1::AcpErrorResponse,
) -> ClientError {
    ClientError::AcpOperation {
        code: error.code,
        message: error.message,
    }
}

fn unexpected_acp_response(operation: &str, response: AcpResponse) -> ClientError {
    ClientError::Sidecar(format!("unexpected response to {operation}: {response:?}"))
}

fn combine_instructions(additional: Option<&str>, binding_reference: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(additional) = additional.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(additional.to_string());
    }
    let binding_reference = binding_reference.trim();
    if !binding_reference.is_empty() {
        parts.push(binding_reference.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn build_binding_reference(bindings: &[Bindings]) -> String {
    if bindings.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        String::from("## Available Host Bindings"),
        String::new(),
        String::from("Run `agentos list-bindings` to see all available bindings."),
        String::new(),
    ];

    for collection in bindings {
        lines.push(format!("### {}", collection.name));
        lines.push(String::new());
        lines.push(collection.description.clone());
        lines.push(String::new());
        for binding in &collection.bindings {
            let signature = build_binding_flag_signature(&binding.input_schema);
            let suffix = if signature.is_empty() {
                String::new()
            } else {
                format!(" {signature}")
            };
            lines.push(format!(
                "- `agentos-{} {}{}` — {}",
                collection.name, binding.name, suffix, binding.description
            ));
        }
        lines.push(String::new());
        lines.push(format!(
            "Run `agentos-{} <binding> --help` for details.",
            collection.name
        ));
        lines.push(String::new());
    }

    lines.join("\n")
}

fn build_binding_flag_signature(schema: &Value) -> String {
    describe_binding_flags(schema)
        .into_iter()
        .map(|flag| {
            if flag.required {
                format!("{} <{}>", flag.name, flag.value_type)
            } else {
                format!("[{} <{}>]", flag.name, flag.value_type)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct BindingFlagDescription {
    name: String,
    value_type: String,
    required: bool,
}

fn describe_binding_flags(schema: &Value) -> Vec<BindingFlagDescription> {
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();

    properties
        .into_iter()
        .map(|(field_name, field_schema)| BindingFlagDescription {
            name: format!("--{}", camel_to_kebab(&field_name)),
            value_type: describe_binding_flag_type(&field_schema),
            required: required.contains(&field_name),
        })
        .collect()
}

fn describe_binding_flag_type(schema: &Value) -> String {
    match json_schema_type(schema) {
        Some("array") => {
            let item_type = schema
                .get("items")
                .and_then(json_schema_type)
                .unwrap_or("string");
            format!("{item_type}[]")
        }
        Some("string") => schema
            .get("enum")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .filter(|values| !values.is_empty())
            .map(|values| values.join("|"))
            .unwrap_or_else(|| String::from("string")),
        Some(other) => other.to_string(),
        None => String::from("string"),
    }
}

fn json_schema_type(schema: &Value) -> Option<&str> {
    schema.get("type").and_then(Value::as_str)
}

fn camel_to_kebab(value: &str) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if ch.is_ascii_uppercase() && index > 0 {
            output.push('-');
        }
        output.push(ch.to_ascii_lowercase());
    }
    output
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

impl AgentOs {
    /// VM-scoped ownership for session RPCs.
    fn session_ownership(&self) -> wire::OwnershipScope {
        wire::OwnershipScope::VmOwnership(wire::VmOwnership {
            connection_id: self.connection_id().to_string(),
            session_id: self.wire_session_id().to_string(),
            vm_id: self.vm_id().to_string(),
        })
    }

    async fn send_acp_request(
        &self,
        request: AcpRequest,
    ) -> std::result::Result<AcpResponse, ClientError> {
        let payload = serde_bare::to_vec(&request).map_err(|error| {
            ClientError::Sidecar(format!("failed to encode ACP request: {error}"))
        })?;
        let response = self
            .transport()
            .request_wire(
                self.session_ownership(),
                wire::RequestPayload::ExtEnvelope(wire::ExtEnvelope {
                    namespace: ACP_EXTENSION_NAMESPACE.to_string(),
                    payload,
                }),
            )
            .await?;
        let envelope = match response {
            wire::ResponsePayload::ExtEnvelope(envelope) => envelope,
            wire::ResponsePayload::RejectedResponse(rejected) => {
                return Err(ClientError::Kernel {
                    code: rejected.code,
                    message: rejected.message,
                });
            }
            other => {
                return Err(ClientError::Sidecar(format!(
                    "unexpected ACP Ext response: {other:?}"
                )));
            }
        };
        if envelope.namespace != ACP_EXTENSION_NAMESPACE {
            return Err(ClientError::Sidecar(format!(
                "unexpected ACP Ext namespace: {}",
                envelope.namespace
            )));
        }
        let response: AcpResponse = serde_bare::from_slice(&envelope.payload).map_err(|error| {
            ClientError::Sidecar(format!("failed to decode ACP response: {error}"))
        })?;
        match response {
            AcpResponse::AcpErrorResponse(error) => Err(ClientError::AcpOperation {
                code: error.code,
                message: error.message,
            }),
            response => Ok(response),
        }
    }

    /// List available agents. A thin forwarder: sends `AcpListAgentsRequest` and
    /// maps the sidecar's response. The sidecar enumerates the projected
    /// `/opt/agentos` packages (client parses no manifests). Every such agent is a
    /// package materialized into the VM at boot, so `installed` is always `true`.
    pub async fn list_agents(&self) -> Result<Vec<AgentRegistryEntry>> {
        let response = self
            .send_acp_request(AcpRequest::AcpListAgentsRequest(AcpListAgentsRequest {
                reserved: false,
            }))
            .await?;
        let AcpResponse::AcpListAgentsResponse(listed) = response else {
            return Err(unexpected_acp_response("AcpListAgentsRequest", response).into());
        };
        Ok(listed
            .agents
            .into_iter()
            .map(|agent| AgentRegistryEntry {
                id: agent.id,
                installed: agent.installed,
            })
            .collect())
    }

    /// Open or restore a durable session. The sidecar owns defaults,
    /// negotiation, and ACP restore selection; an omitted ID targets `main`.
    /// This is an idempotent command and returns no metadata. Call
    /// [`AgentOs::get_session`] when the stored session record is needed.
    pub async fn open_session(&self, input: OpenSessionInput) -> Result<()> {
        let caller_instructions = [
            self.config().additional_instructions.as_deref(),
            input.additional_instructions.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        let binding_reference = build_binding_reference(&self.config().bindings);
        let additional_instructions = combine_instructions(
            (!caller_instructions.is_empty()).then_some(caller_instructions.as_str()),
            &binding_reference,
        );
        let response = self
            .send_acp_request(AcpRequest::AcpOpenSessionRequest(AcpOpenSessionRequest {
                session_id: input.session_id,
                agent: input.agent,
                cwd: input.cwd,
                additional_directories: encode_optional_json(input.additional_directories)?,
                env: encode_optional_json(input.env)?,
                mcp_servers: encode_optional_json(input.mcp_servers)?,
                permission_policy: input.permission_policy.map(permission_policy_wire),
                skip_os_instructions: input.skip_os_instructions,
                additional_instructions,
            }))
            .await?;
        match response {
            AcpResponse::AcpOpenSessionResponse(_) => Ok(()),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpOpenSessionRequest", other).into()),
        }
    }

    /// Read durable metadata without starting or restoring an ACP adapter.
    pub async fn get_session(&self, session_id: Option<&str>) -> Result<SessionInfo> {
        let response = self
            .send_acp_request(AcpRequest::AcpGetDurableSessionRequest(
                AcpGetDurableSessionRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpGetDurableSessionResponse(response) => {
                decode_session_info(response.session)
            }
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpGetDurableSessionRequest", other).into()),
        }
    }

    /// Traverse durable sessions by the sidecar-issued keyset cursor. This is a
    /// SQLite-only operation and never starts an adapter.
    pub async fn list_sessions(&self, input: ListSessionsInput) -> Result<SessionPage> {
        let response = self
            .send_acp_request(AcpRequest::AcpListDurableSessionsRequest(
                AcpListDurableSessionsRequest {
                    cursor: input.cursor,
                    limit: input.limit,
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpListDurableSessionsResponse(response) => Ok(SessionPage {
                sessions: response
                    .sessions
                    .into_iter()
                    .map(decode_session_info)
                    .collect::<Result<Vec<_>>>()?,
                next_cursor: response.next_cursor,
            }),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpListDurableSessionsRequest", other).into()),
        }
    }

    /// Permanently delete durable metadata and history. `None` targets `main`.
    pub async fn delete_session(&self, session_id: Option<&str>) -> Result<()> {
        let response = self
            .send_acp_request(AcpRequest::AcpDeleteSessionRequest(
                AcpDeleteSessionRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpDeleteSessionResponse(_) => Ok(()),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpDeleteSessionRequest", other).into()),
        }
    }

    /// Release the live adapter while preserving the durable session.
    pub async fn unload_session(&self, session_id: Option<&str>) -> Result<()> {
        let response = self
            .send_acp_request(AcpRequest::AcpUnloadSessionRequest(
                AcpUnloadSessionRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpUnloadSessionResponse(_) => Ok(()),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpUnloadSessionRequest", other).into()),
        }
    }

    /// Durably accept a complete ACP prompt before dispatch. A missing session
    /// is an error; the sidecar never creates one here or retries uncertain work.
    pub async fn prompt(&self, input: PromptInput) -> Result<PromptResult> {
        let response = self
            .send_acp_request(AcpRequest::AcpPromptRequest(AcpPromptRequest {
                session_id: input.session_id,
                idempotency_key: input.idempotency_key,
                content: encode_json(&input.content)?,
            }))
            .await?;
        match response {
            AcpResponse::AcpPromptResponse(response) => Ok(PromptResult {
                session_id: response.session_id,
                message: decode_optional_json(response.message)?,
                stop_reason: decode_json(&serde_json::to_string(&response.stop_reason)?)?,
            }),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpPromptRequest", other).into()),
        }
    }

    pub async fn cancel_prompt(&self, session_id: Option<&str>) -> Result<CancelPromptStatus> {
        let response = self
            .send_acp_request(AcpRequest::AcpCancelPromptRequest(AcpCancelPromptRequest {
                session_id: session_id.map(ToOwned::to_owned),
            }))
            .await?;
        match response {
            AcpResponse::AcpCancelPromptResponse(response) => match response.status.as_str() {
                "cancelled" => Ok(CancelPromptStatus::Cancelled),
                "no_active_prompt" => Ok(CancelPromptStatus::NoActivePrompt),
                status => Err(ClientError::Sidecar(format!(
                    "invalid cancelPrompt status: {status}"
                ))
                .into()),
            },
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpCancelPromptRequest", other).into()),
        }
    }

    pub async fn respond_permission(
        &self,
        session_id: &str,
        request_id: &str,
        option_id: &str,
    ) -> Result<PermissionResponseStatus> {
        let response = self
            .send_acp_request(AcpRequest::AcpRespondPermissionRequest(
                AcpRespondPermissionRequest {
                    session_id: session_id.to_owned(),
                    request_id: request_id.to_owned(),
                    option_id: option_id.to_owned(),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpRespondPermissionResponse(response) => match response.status.as_str() {
                "accepted" => Ok(PermissionResponseStatus::Accepted),
                "not_pending" => Ok(PermissionResponseStatus::NotPending(
                    permission_terminal_reason(
                        response.reason.as_deref().unwrap_or("request_not_found"),
                    )?,
                )),
                status => Err(ClientError::Sidecar(format!(
                    "invalid respondPermission status: {status}"
                ))
                .into()),
            },
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpRespondPermissionRequest", other).into()),
        }
    }

    /// Read the authoritative SQLite history. ACP message updates deserialize
    /// through the official protocol-v1 schema crate.
    pub async fn read_history(&self, input: ReadHistoryInput) -> Result<HistoryPage> {
        let response = self
            .send_acp_request(AcpRequest::AcpReadHistoryRequest(AcpReadHistoryRequest {
                session_id: input.session_id,
                before: input.before,
                after: input.after,
                limit: input.limit,
            }))
            .await?;
        match response {
            AcpResponse::AcpHistoryPageResponse(response) => Ok(HistoryPage {
                events: response
                    .events
                    .into_iter()
                    .map(decode_history_entry)
                    .collect::<Result<Vec<_>>>()?,
                has_more_before: response.has_more_before,
                has_more_after: response.has_more_after,
            }),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpReadHistoryRequest", other).into()),
        }
    }

    pub async fn get_session_config(&self, session_id: Option<&str>) -> Result<SessionConfig> {
        let response = self
            .send_acp_request(AcpRequest::AcpGetSessionConfigRequest(
                AcpGetSessionConfigRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        decode_session_config_response(response, "AcpGetSessionConfigRequest")
    }

    pub async fn set_session_config_option(
        &self,
        session_id: Option<&str>,
        config_id: &str,
        value: SessionConfigValue,
    ) -> Result<SessionConfig> {
        let response = self
            .send_acp_request(AcpRequest::AcpSetSessionConfigOptionRequest(
                AcpSetSessionConfigOptionRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                    config_id: config_id.to_owned(),
                    value: encode_json(&value)?,
                },
            ))
            .await?;
        decode_session_config_response(response, "AcpSetSessionConfigOptionRequest")
    }

    pub async fn get_session_capabilities(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<SessionCapabilities>> {
        let response = self
            .send_acp_request(AcpRequest::AcpGetSessionCapabilitiesRequest(
                AcpGetSessionCapabilitiesRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpSessionCapabilitiesResponse(response) => response
                .capabilities
                .map(|raw| normalize_session_capabilities(decode_json(&raw)?))
                .transpose(),
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpGetSessionCapabilitiesRequest", other).into()),
        }
    }

    pub async fn get_session_agent_info(
        &self,
        session_id: Option<&str>,
    ) -> Result<Option<agent_client_protocol_schema::v1::Implementation>> {
        let response = self
            .send_acp_request(AcpRequest::AcpGetSessionAgentInfoRequest(
                AcpGetSessionAgentInfoRequest {
                    session_id: session_id.map(ToOwned::to_owned),
                },
            ))
            .await?;
        match response {
            AcpResponse::AcpSessionAgentInfoResponse(response) => {
                decode_optional_json(response.agent_info)
            }
            AcpResponse::AcpErrorResponse(error) => Err(acp_operation_error(error).into()),
            other => Err(unexpected_acp_response("AcpGetSessionAgentInfoRequest", other).into()),
        }
    }

    /// Subscribe to durable and ephemeral ACP updates for one public session.
    /// Omitted session IDs target `main`. Durable entries are emitted only
    /// after their SQLite transaction commits; ephemeral entries are deltas and
    /// have no durable sequence of their own.
    pub fn on_session_event(&self, session_id: Option<&str>) -> DurableSessionEventSubscription {
        let session_id = session_id.unwrap_or("main").to_owned();
        let rx = self.inner().durable_session_event_tx.subscribe();
        let stream = futures::stream::unfold((rx, session_id), |(mut rx, session_id)| async move {
            loop {
                match rx.recv().await {
                    Ok(entry) if session_stream_id(&entry) == session_id => {
                        return Some((Ok(entry), (rx, session_id)));
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        return Some((
                            Err(SessionSubscriptionError::Lagged { skipped }),
                            (rx, session_id),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        (Box::pin(stream), Subscription::noop())
    }

    /// Subscribe to unexpected ACP adapter exits. Pass a session ID to filter
    /// to one durable session, or `None` to observe all adapter exits for this
    /// VM. Subscribing never starts or restores an adapter.
    pub fn on_agent_exit(&self, session_id: Option<&str>) -> AgentExitSubscription {
        let session_id = session_id.map(ToOwned::to_owned);
        let rx = self.inner().durable_agent_exit_tx.subscribe();
        let stream = futures::stream::unfold((rx, session_id), |(mut rx, session_id)| async move {
            loop {
                match rx.recv().await {
                    Ok(event)
                        if session_id
                            .as_ref()
                            .is_none_or(|expected| expected == &event.session_id) =>
                    {
                        return Some((event, (rx, session_id)));
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "lagged durable ACP agent-exit subscription");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        (Box::pin(stream), Subscription::noop())
    }
}

fn permission_terminal_reason(reason: &str) -> Result<PermissionTerminalReason> {
    Ok(match reason {
        "already_resolved" => PermissionTerminalReason::AlreadyResolved,
        "prompt_cancelled" => PermissionTerminalReason::PromptCancelled,
        "adapter_exited" => PermissionTerminalReason::AdapterExited,
        "session_deleted" => PermissionTerminalReason::SessionDeleted,
        "vm_shutdown" => PermissionTerminalReason::VmShutdown,
        "request_not_found" => PermissionTerminalReason::RequestNotFound,
        other => anyhow::bail!("invalid permission terminal reason: {other}"),
    })
}
