use crate::acp::compat::{
    compatibility_for, derive_config_options, synthetic_mode_update, AgentCompatibilityKind,
    PendingPermissionRequest, RECENT_ACTIVITY_LIMIT,
};
use crate::acp::{JsonRpcId, JsonRpcNotification};
use crate::protocol::{SequencedNotification, SessionCreatedResponse, SessionStateResponse};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    pub(crate) events: Vec<SequencedEvent>,
    pub(crate) modes: Option<Value>,
    pub(crate) config_options: Vec<Value>,
    pub(crate) agent_capabilities: Option<Value>,
    pub(crate) agent_info: Option<Value>,
    pub(crate) recent_activity: VecDeque<String>,
    pub(crate) pending_permission_requests: BTreeMap<String, PendingPermissionRequest>,
    pub(crate) seen_inbound_request_ids: BTreeSet<JsonRpcId>,
    pub(crate) terminals: BTreeMap<String, AcpTerminalState>,
    pub(crate) next_terminal_id: u64,
    pub(crate) closed: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) compatibility: AgentCompatibilityKind,
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
        let compatibility = compatibility_for(&agent_type);
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
            events: Vec::new(),
            modes: session_result
                .get("modes")
                .cloned()
                .or_else(|| init_result.get("modes").cloned()),
            config_options,
            agent_capabilities: init_result.get("agentCapabilities").cloned(),
            agent_info: init_result.get("agentInfo").cloned(),
            recent_activity: VecDeque::with_capacity(RECENT_ACTIVITY_LIMIT),
            pending_permission_requests: BTreeMap::new(),
            seen_inbound_request_ids: BTreeSet::new(),
            terminals: BTreeMap::new(),
            next_terminal_id: 1,
            closed: false,
            exit_code: None,
            compatibility,
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

    pub(crate) fn state_response(&self) -> SessionStateResponse {
        SessionStateResponse {
            session_id: self.session_id.clone(),
            agent_type: self.agent_type.clone(),
            process_id: self.process_id.clone(),
            pid: self.pid,
            closed: self.closed,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
            events: self
                .events
                .iter()
                .map(|event| SequencedNotification {
                    sequence_number: event.sequence_number,
                    notification: serde_json::to_value(&event.notification)
                        .expect("serialize ACP notification"),
                })
                .collect(),
        }
    }

    pub(crate) fn record_activity(&mut self, entry: String) {
        self.recent_activity.push_back(entry);
        while self.recent_activity.len() > RECENT_ACTIVITY_LIMIT {
            self.recent_activity.pop_front();
        }
    }

    pub(crate) fn record_notification(&mut self, notification: JsonRpcNotification) {
        self.apply_session_update(&notification);
        self.events.push(SequencedEvent {
            sequence_number: self.next_sequence_number,
            notification,
        });
        self.next_sequence_number += 1;
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
        event_count_before: usize,
    ) -> Option<JsonRpcNotification> {
        if method == "session/set_mode" {
            if let Some(mode_id) = params.get("modeId").and_then(Value::as_str) {
                self.apply_local_mode_update(mode_id);
                if matches!(self.compatibility, AgentCompatibilityKind::OpenCode)
                    && !self.has_session_update_since(event_count_before, |update| {
                        update
                            .get("sessionUpdate")
                            .and_then(Value::as_str)
                            .is_some_and(|value| value == "current_mode_update")
                            && update
                                .get("currentModeId")
                                .and_then(Value::as_str)
                                .is_some_and(|value| value == mode_id)
                    })
                {
                    let notification = synthetic_mode_update(mode_id);
                    self.record_notification(notification.clone());
                    return Some(notification);
                }
            }
        }

        if method == "session/set_config_option" {
            if let (Some(config_id), Some(value)) = (
                params.get("configId").and_then(Value::as_str),
                params.get("value").and_then(Value::as_str),
            ) {
                self.apply_local_config_update(config_id, value);
            }
        }

        None
    }

    fn has_session_update_since(
        &self,
        start_index: usize,
        predicate: impl Fn(&Map<String, Value>) -> bool,
    ) -> bool {
        self.events.iter().skip(start_index).any(|event| {
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

    fn apply_local_config_update(&mut self, config_id: &str, value: &str) {
        self.config_options = self
            .config_options
            .iter()
            .map(|option| {
                let Some(mut map) = option.as_object().cloned() else {
                    return option.clone();
                };
                let is_target = map
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == config_id);
                if is_target {
                    map.insert(
                        String::from("currentValue"),
                        Value::String(String::from(value)),
                    );
                }
                Value::Object(map)
            })
            .collect();
    }
}
