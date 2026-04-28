use crate::acp::compat::{
    derive_config_options, synthetic_config_update, synthetic_mode_update,
    PendingPermissionRequest, SeenInboundRequestIds, RECENT_ACTIVITY_LIMIT,
};
use crate::acp::AcpTimeoutDiagnostics;
use crate::acp::{JsonRpcError, JsonRpcId, JsonRpcNotification};
use crate::protocol::{SequencedNotification, SessionCreatedResponse, SessionStateResponse};
use serde::Serialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpSessionStateError {
    NotificationSerialization(String),
    InvalidConfigOptionParams(String),
    MalformedConfigOptionEntry { index: usize, reason: String },
    UnknownConfigOption(String),
}

impl AcpSessionStateError {
    fn notification_serialization(error: serde_json::Error) -> Self {
        Self::NotificationSerialization(format!("failed to serialize ACP notification: {error}"))
    }

    fn invalid_config_option_params(message: impl Into<String>) -> Self {
        Self::InvalidConfigOptionParams(message.into())
    }

    fn malformed_config_option_entry(index: usize, reason: impl Into<String>) -> Self {
        Self::MalformedConfigOptionEntry {
            index,
            reason: reason.into(),
        }
    }

    fn unknown_config_option(config_id: impl Into<String>) -> Self {
        Self::UnknownConfigOption(config_id.into())
    }

    pub(crate) fn to_json_rpc_error(&self, method: &str) -> JsonRpcError {
        match self {
            Self::NotificationSerialization(message) => JsonRpcError {
                code: -32000,
                message: message.clone(),
                data: Some(serde_json::json!({
                    "kind": "acp_session_state",
                    "method": method,
                })),
            },
            Self::InvalidConfigOptionParams(message) => JsonRpcError {
                code: -32602,
                message: format!("Invalid params for {method}: {message}"),
                data: Some(serde_json::json!({
                    "kind": "invalid_config_option_params",
                    "method": method,
                })),
            },
            Self::MalformedConfigOptionEntry { index, reason } => JsonRpcError {
                code: -32602,
                message: format!(
                    "Invalid params for {method}: config option entry {index} is malformed: {reason}"
                ),
                data: Some(serde_json::json!({
                    "kind": "malformed_config_option_entry",
                    "method": method,
                    "index": index,
                })),
            },
            Self::UnknownConfigOption(config_id) => JsonRpcError {
                code: -32602,
                message: format!("Invalid params for {method}: unknown config option {config_id}"),
                data: Some(serde_json::json!({
                    "kind": "unknown_config_option",
                    "method": method,
                    "configId": config_id,
                })),
            },
        }
    }
}

impl fmt::Display for AcpSessionStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotificationSerialization(message) | Self::InvalidConfigOptionParams(message) => {
                f.write_str(message)
            }
            Self::MalformedConfigOptionEntry { index, reason } => {
                write!(f, "config option entry {index} is malformed: {reason}")
            }
            Self::UnknownConfigOption(config_id) => {
                write!(f, "unknown config option {config_id}")
            }
        }
    }
}

impl std::error::Error for AcpSessionStateError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpInitializeError {
    MissingProtocolVersion,
    InvalidProtocolVersion,
    ProtocolVersionMismatch { requested: u64, reported: u64 },
}

impl fmt::Display for AcpInitializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProtocolVersion => {
                f.write_str("ACP initialize response missing protocolVersion")
            }
            Self::InvalidProtocolVersion => {
                f.write_str("ACP initialize response protocolVersion must be an unsigned integer")
            }
            Self::ProtocolVersionMismatch {
                requested,
                reported,
            } => write!(
                f,
                "ACP initialize protocolVersion mismatch: requested {requested}, agent reported {reported}"
            ),
        }
    }
}

impl std::error::Error for AcpInitializeError {}

pub(crate) fn build_initialize_request(
    protocol_version: u64,
    client_capabilities: Value,
) -> crate::acp::JsonRpcRequest {
    crate::acp::JsonRpcRequest {
        jsonrpc: String::from("2.0"),
        id: JsonRpcId::Number(1),
        method: String::from("initialize"),
        params: Some(serde_json::json!({
            "protocolVersion": protocol_version,
            "clientCapabilities": client_capabilities,
        })),
    }
}

pub(crate) fn validate_initialize_result(
    init_result: &Map<String, Value>,
    requested_protocol_version: u64,
) -> Result<u64, AcpInitializeError> {
    let reported_protocol_version = init_result
        .get("protocolVersion")
        .ok_or(AcpInitializeError::MissingProtocolVersion)?
        .as_u64()
        .ok_or(AcpInitializeError::InvalidProtocolVersion)?;

    if reported_protocol_version != requested_protocol_version {
        return Err(AcpInitializeError::ProtocolVersionMismatch {
            requested: requested_protocol_version,
            reported: reported_protocol_version,
        });
    }

    Ok(reported_protocol_version)
}

#[derive(Debug, Clone)]
pub(crate) struct AcpTerminalState {
    pub(crate) process_id: String,
    pub(crate) output: String,
    pub(crate) truncated: bool,
    pub(crate) output_byte_limit: usize,
    pub(crate) exit_code: Option<i32>,
    pub(crate) released: bool,
}

impl AcpTerminalState {
    pub(crate) fn new(process_id: String, output_byte_limit: usize) -> Self {
        Self {
            process_id,
            output: String::new(),
            truncated: false,
            output_byte_limit,
            exit_code: None,
            released: false,
        }
    }

    pub(crate) fn append_output(&mut self, chunk: &[u8]) {
        self.output.push_str(&String::from_utf8_lossy(chunk));
        if self.output_byte_limit == 0 {
            self.output.clear();
            self.truncated = true;
            return;
        }

        while self.output.len() > self.output_byte_limit {
            let remove_len = self
                .output
                .chars()
                .next()
                .map(char::len_utf8)
                .unwrap_or(self.output.len());
            self.output.drain(..remove_len);
            self.truncated = true;
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SequencedEvent {
    pub(crate) sequence_number: u64,
    pub(crate) notification: JsonRpcNotification,
}

pub(crate) const ACP_SESSION_EVENT_RETENTION_LIMIT: usize = 1024;

#[derive(Debug, Clone)]
pub(crate) struct AcpSessionState {
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
    pub(crate) agent_type: String,
    pub(crate) process_id: String,
    pub(crate) pid: Option<u32>,
    pub(crate) stdout_buffer: String,
    pub(crate) next_request_id: i64,
    pub(crate) next_sequence_number: u64,
    pub(crate) events: VecDeque<SequencedEvent>,
    pub(crate) modes: Option<Value>,
    pub(crate) config_options: Vec<Value>,
    pub(crate) agent_capabilities: Option<Value>,
    pub(crate) agent_info: Option<Value>,
    pub(crate) recent_activity: VecDeque<String>,
    pub(crate) pending_permission_requests: BTreeMap<String, PendingPermissionRequest>,
    pub(crate) seen_inbound_request_ids: SeenInboundRequestIds,
    pub(crate) terminals: BTreeMap<String, AcpTerminalState>,
    pub(crate) next_terminal_id: u64,
    pub(crate) closed: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) termination_requested: bool,
}

impl AcpSessionState {
    pub(crate) fn new(
        session_id: String,
        vm_id: String,
        agent_type: String,
        process_id: String,
        pid: Option<u32>,
        init_result: &Map<String, Value>,
        session_result: &Map<String, Value>,
    ) -> Self {
        let mut config_options = init_result
            .get("configOptions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(overrides) = session_result
            .get("configOptions")
            .and_then(Value::as_array)
        {
            config_options = overrides.clone();
        }
        let has_model_option = config_options.iter().any(|option| {
            option.as_object().is_some_and(|map| {
                map.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == "model")
            })
        });
        if !has_model_option {
            config_options.extend(derive_config_options(&agent_type, session_result));
        }

        Self {
            session_id,
            vm_id,
            agent_type,
            process_id,
            pid,
            stdout_buffer: String::new(),
            // The sidecar already used request ids 1 and 2 on this ACP
            // connection for initialize and session/new before the session
            // state is created. Continue from 3 so later session RPCs never
            // reuse ids on the same transport.
            next_request_id: 3,
            next_sequence_number: 0,
            events: VecDeque::with_capacity(ACP_SESSION_EVENT_RETENTION_LIMIT),
            modes: session_result
                .get("modes")
                .cloned()
                .or_else(|| init_result.get("modes").cloned()),
            config_options,
            agent_capabilities: init_result.get("agentCapabilities").cloned(),
            agent_info: init_result.get("agentInfo").cloned(),
            recent_activity: VecDeque::with_capacity(RECENT_ACTIVITY_LIMIT),
            pending_permission_requests: BTreeMap::new(),
            seen_inbound_request_ids: SeenInboundRequestIds::default(),
            terminals: BTreeMap::new(),
            next_terminal_id: 1,
            closed: false,
            exit_code: None,
            termination_requested: false,
        }
    }

    pub(crate) fn created_response(&self) -> SessionCreatedResponse {
        SessionCreatedResponse {
            session_id: self.session_id.clone(),
            pid: self.pid,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn state_response(&self) -> Result<SessionStateResponse, AcpSessionStateError> {
        self.state_response_with_additional_events(std::iter::empty())
    }

    pub(crate) fn acknowledged_state_response(
        &mut self,
        acknowledged_sequence_number: Option<u64>,
    ) -> Result<SessionStateResponse, AcpSessionStateError> {
        self.drain_acknowledged_events(acknowledged_sequence_number);
        self.state_response_with_additional_events(std::iter::empty())
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn state_response_with_test_notification<T: Serialize>(
        &self,
        sequence_number: u64,
        notification: &T,
    ) -> Result<SessionStateResponse, AcpSessionStateError> {
        self.state_response_with_additional_events(std::iter::once(
            serialize_sequenced_notification(sequence_number, notification),
        ))
    }

    fn state_response_with_additional_events<I>(
        &self,
        additional_events: I,
    ) -> Result<SessionStateResponse, AcpSessionStateError>
    where
        I: IntoIterator<Item = Result<SequencedNotification, AcpSessionStateError>>,
    {
        let mut events = self
            .events
            .iter()
            .map(|event| {
                serialize_sequenced_notification(event.sequence_number, &event.notification)
            })
            .collect::<Result<Vec<_>, _>>()?;
        events.extend(
            additional_events
                .into_iter()
                .collect::<Result<Vec<_>, _>>()?,
        );

        Ok(SessionStateResponse {
            session_id: self.session_id.clone(),
            agent_type: self.agent_type.clone(),
            process_id: self.process_id.clone(),
            pid: self.pid,
            closed: self.closed,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
            events,
        })
    }

    pub(crate) fn record_activity(&mut self, entry: String) {
        self.recent_activity.push_back(entry);
        while self.recent_activity.len() > RECENT_ACTIVITY_LIMIT {
            self.recent_activity.pop_front();
        }
    }

    pub(crate) fn mark_termination_requested(&mut self) {
        self.termination_requested = true;
        self.closed = true;
    }

    pub(crate) fn timeout_diagnostics(
        &self,
        method: &str,
        id: &JsonRpcId,
        timeout_ms: u64,
        transport_state: Option<String>,
    ) -> AcpTimeoutDiagnostics {
        AcpTimeoutDiagnostics::new(
            method,
            id.clone(),
            timeout_ms,
            self.exit_code,
            self.timeout_killed_state(),
            transport_state,
            self.recent_activity.iter().cloned().collect(),
        )
    }

    pub(crate) fn record_notification(&mut self, notification: JsonRpcNotification) {
        self.apply_session_update(&notification);
        self.events.push_back(SequencedEvent {
            sequence_number: self.next_sequence_number,
            notification,
        });
        self.next_sequence_number += 1;
        self.trim_event_buffer();
    }

    pub(crate) fn allocate_terminal_id(&mut self) -> String {
        let terminal_id = format!("acp-term-{}", self.next_terminal_id);
        self.next_terminal_id += 1;
        terminal_id
    }

    pub(crate) fn apply_request_success(
        &mut self,
        method: &str,
        params: &Map<String, Value>,
        sequence_number_before: u64,
    ) -> Result<Option<JsonRpcNotification>, AcpSessionStateError> {
        if method == "session/set_mode" {
            if let Some(mode_id) = params.get("modeId").and_then(Value::as_str) {
                self.apply_local_mode_update(mode_id);
                if !self.has_session_update_since(sequence_number_before, |update| {
                    update
                        .get("sessionUpdate")
                        .and_then(Value::as_str)
                        .is_some_and(|value| value == "current_mode_update")
                        && update
                            .get("currentModeId")
                            .and_then(Value::as_str)
                            .is_some_and(|value| value == mode_id)
                }) {
                    let notification = synthetic_mode_update(mode_id);
                    self.record_notification(notification.clone());
                    return Ok(Some(notification));
                }
            }
        }

        if method == "session/set_config_option" {
            let config_id = params
                .get("configId")
                .ok_or_else(|| {
                    AcpSessionStateError::invalid_config_option_params("configId is required")
                })?
                .as_str()
                .ok_or_else(|| {
                    AcpSessionStateError::invalid_config_option_params("configId must be a string")
                })?;
            let value = params.get("value").ok_or_else(|| {
                AcpSessionStateError::invalid_config_option_params("value is required")
            })?;
            self.apply_local_config_update(config_id, value)?;
            if !self.has_session_update_since(sequence_number_before, |update| {
                update
                    .get("sessionUpdate")
                    .and_then(Value::as_str)
                    .is_some_and(|value| {
                        value == "config_option_update" || value == "config_options_update"
                    })
            }) {
                let notification = synthetic_config_update(&self.config_options);
                self.record_notification(notification.clone());
                return Ok(Some(notification));
            }
        }

        Ok(None)
    }

    fn has_session_update_since(
        &self,
        sequence_number_before: u64,
        predicate: impl Fn(&Map<String, Value>) -> bool,
    ) -> bool {
        self.events.iter().any(|event| {
            if event.sequence_number < sequence_number_before {
                return false;
            }
            if event.notification.method != "session/update" {
                return false;
            }
            let params = event
                .notification
                .params
                .clone()
                .and_then(|value| value.as_object().cloned());
            let Some(params) = params else {
                return false;
            };
            let update = params
                .get("update")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or(params);
            predicate(&update)
        })
    }

    fn drain_acknowledged_events(&mut self, acknowledged_sequence_number: Option<u64>) {
        let Some(acknowledged_sequence_number) = acknowledged_sequence_number else {
            return;
        };
        while self
            .events
            .front()
            .is_some_and(|event| event.sequence_number <= acknowledged_sequence_number)
        {
            self.events.pop_front();
        }
    }

    fn trim_event_buffer(&mut self) {
        while self.events.len() > ACP_SESSION_EVENT_RETENTION_LIMIT {
            self.events.pop_front();
        }
    }

    fn apply_session_update(&mut self, notification: &JsonRpcNotification) {
        if notification.method != "session/update" {
            return;
        }
        let Some(params) = notification
            .params
            .clone()
            .and_then(|value| value.as_object().cloned())
        else {
            return;
        };
        let update = params
            .get("update")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or(params);

        if update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .is_some_and(|value| value == "current_mode_update")
        {
            if let Some(current_mode_id) = update.get("currentModeId").and_then(Value::as_str) {
                self.apply_local_mode_update(current_mode_id);
            }
        }

        if update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                value == "config_option_update" || value == "config_options_update"
            })
        {
            if let Some(config_options) = update.get("configOptions").and_then(Value::as_array) {
                self.config_options = config_options.clone();
            }
        }
    }

    fn apply_local_mode_update(&mut self, mode_id: &str) {
        let Some(Value::Object(modes)) = self.modes.as_mut() else {
            return;
        };
        modes.insert(
            String::from("currentModeId"),
            Value::String(String::from(mode_id)),
        );
    }

    fn apply_local_config_update(
        &mut self,
        config_id: &str,
        value: &Value,
    ) -> Result<(), AcpSessionStateError> {
        let mut updated = false;
        let mut config_options = Vec::with_capacity(self.config_options.len());
        for (index, option) in self.config_options.iter().enumerate() {
            let mut map = option.as_object().cloned().ok_or_else(|| {
                AcpSessionStateError::malformed_config_option_entry(index, "expected an object")
            })?;
            let option_id = map.get("id").and_then(Value::as_str).ok_or_else(|| {
                AcpSessionStateError::malformed_config_option_entry(index, "missing string id")
            })?;
            if option_id == config_id {
                map.insert(String::from("currentValue"), value.clone());
                updated = true;
            }
            config_options.push(Value::Object(map));
        }
        if !updated {
            return Err(AcpSessionStateError::unknown_config_option(config_id));
        }
        self.config_options = config_options;
        Ok(())
    }

    fn timeout_killed_state(&self) -> Option<bool> {
        if self.exit_code.is_some() {
            return Some(self.termination_requested);
        }
        self.termination_requested.then_some(true)
    }
}

fn serialize_sequenced_notification<T: Serialize>(
    sequence_number: u64,
    notification: &T,
) -> Result<SequencedNotification, AcpSessionStateError> {
    Ok(SequencedNotification {
        sequence_number,
        notification: serde_json::to_value(notification)
            .map_err(AcpSessionStateError::notification_serialization)?,
    })
}
