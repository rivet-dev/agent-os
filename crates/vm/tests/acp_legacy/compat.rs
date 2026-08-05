use crate::json_rpc::{JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(crate) const LEGACY_PERMISSION_METHOD: &str = "request/permission";
pub(crate) const ACP_PERMISSION_METHOD: &str = "session/request_permission";
pub(crate) const ACP_CANCEL_METHOD: &str = "session/cancel";
pub(crate) const RECENT_ACTIVITY_LIMIT: usize = 20;
pub(crate) const ACTIVITY_TEXT_LIMIT: usize = 240;
pub(crate) const SEEN_INBOUND_REQUEST_ID_RETENTION_LIMIT: usize = 4_096;
pub(crate) const PENDING_PERMISSION_REQUEST_RETENTION_LIMIT: usize = 4_096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentCompatibilityKind {
    Generic,
    OpenCode,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingPermissionRequest {
    pub(crate) id: JsonRpcId,
    pub(crate) method: String,
    pub(crate) options: Option<Vec<Map<String, Value>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingPermissionRequests {
    pending: BTreeMap<JsonRpcId, PendingPermissionRequest>,
    permission_ids: BTreeMap<String, JsonRpcId>,
    order: VecDeque<JsonRpcId>,
    limit: usize,
}

impl PendingPermissionRequests {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            pending: BTreeMap::new(),
            permission_ids: BTreeMap::new(),
            order: VecDeque::new(),
            limit,
        }
    }

    pub(crate) fn insert(&mut self, request: PendingPermissionRequest) -> String {
        self.remove_existing_permission_id(&request.id);
        if !self.pending.contains_key(&request.id) {
            self.order.push_back(request.id.clone());
        }
        let permission_id = self.assign_permission_id(&request.id);
        let request_id = request.id.clone();
        self.pending.insert(request.id.clone(), request);
        self.permission_ids
            .insert(permission_id.clone(), request_id);
        self.evict_oldest();
        permission_id
    }

    pub(crate) fn remove_by_permission_id(
        &mut self,
        permission_id: &str,
    ) -> Option<PendingPermissionRequest> {
        let id = self.permission_ids.remove(permission_id)?;
        self.order.retain(|existing| existing != &id);
        self.pending.remove(&id)
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.permission_ids.clear();
        self.order.clear();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub(crate) fn contains_id(&self, id: &JsonRpcId) -> bool {
        self.pending.contains_key(id)
    }

    fn evict_oldest(&mut self) {
        while self.order.len() > self.limit {
            if let Some(oldest) = self.order.pop_front() {
                self.pending.remove(&oldest);
                self.remove_existing_permission_id(&oldest);
            }
        }
    }

    fn assign_permission_id(&self, id: &JsonRpcId) -> String {
        let display_id = id.to_string();
        if !self.permission_ids.contains_key(&display_id) {
            return display_id;
        }

        let encoded = serde_json::to_string(id).expect("JSON-RPC id should serialize");
        let mut candidate = format!("jsonrpc:{encoded}");
        let mut suffix = 2usize;
        while self.permission_ids.contains_key(&candidate) {
            candidate = format!("jsonrpc:{encoded}:{suffix}");
            suffix += 1;
        }
        candidate
    }

    fn remove_existing_permission_id(&mut self, id: &JsonRpcId) {
        let existing = self
            .permission_ids
            .iter()
            .find_map(|(permission_id, pending_id)| {
                if pending_id == id {
                    Some(permission_id.clone())
                } else {
                    None
                }
            });
        if let Some(permission_id) = existing {
            self.permission_ids.remove(&permission_id);
        }
    }
}

impl Default for PendingPermissionRequests {
    fn default() -> Self {
        Self::new(PENDING_PERMISSION_REQUEST_RETENTION_LIMIT)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SeenInboundRequestIds {
    seen: BTreeSet<JsonRpcId>,
    order: VecDeque<JsonRpcId>,
    limit: usize,
}

impl SeenInboundRequestIds {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            seen: BTreeSet::new(),
            order: VecDeque::new(),
            limit,
        }
    }

    pub(crate) fn contains(&self, id: &JsonRpcId) -> bool {
        self.seen.contains(id)
    }

    pub(crate) fn insert(&mut self, id: JsonRpcId) {
        if !self.seen.insert(id.clone()) {
            return;
        }
        self.order.push_back(id);
        self.evict_oldest();
    }

    pub(crate) fn clear(&mut self) {
        self.seen.clear();
        self.order.clear();
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn len(&self) -> usize {
        self.seen.len()
    }

    fn evict_oldest(&mut self) {
        while self.order.len() > self.limit {
            if let Some(oldest) = self.order.pop_front() {
                self.seen.remove(&oldest);
            }
        }
    }
}

impl Default for SeenInboundRequestIds {
    fn default() -> Self {
        Self::new(SEEN_INBOUND_REQUEST_ID_RETENTION_LIMIT)
    }
}

pub(crate) fn compatibility_for(agent_type: &str) -> AgentCompatibilityKind {
    match agent_type {
        "opencode" => AgentCompatibilityKind::OpenCode,
        _ => AgentCompatibilityKind::Generic,
    }
}

pub(crate) fn normalize_inbound_permission_request(
    request: &JsonRpcRequest,
    seen_inbound_request_ids: &mut SeenInboundRequestIds,
    pending_permission_requests: &mut PendingPermissionRequests,
) -> Option<JsonRpcNotification> {
    if request.method != ACP_PERMISSION_METHOD {
        return None;
    }

    if seen_inbound_request_ids.contains(&request.id) {
        return None;
    }
    seen_inbound_request_ids.insert(request.id.clone());

    let params = to_record(request.params.clone());
    let permission_id = pending_permission_requests.insert(PendingPermissionRequest {
        id: request.id.clone(),
        method: request.method.clone(),
        options: params
            .get("options")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_object)
                    .cloned()
                    .collect::<Vec<_>>()
            }),
    });

    let mut normalized = params;
    normalized.insert(String::from("permissionId"), Value::String(permission_id));
    normalized.insert(
        String::from("_acpMethod"),
        Value::String(request.method.clone()),
    );
    Some(JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from(LEGACY_PERMISSION_METHOD),
        params: Some(Value::Object(normalized)),
    })
}

pub(crate) fn maybe_normalize_permission_response(
    method: &str,
    params: Option<Value>,
    pending_permission_requests: &mut PendingPermissionRequests,
) -> Option<(JsonRpcId, Value)> {
    if method != LEGACY_PERMISSION_METHOD && method != ACP_PERMISSION_METHOD {
        return None;
    }

    let payload = to_record(params);
    let permission_id = match payload.get("permissionId") {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        _ => return None,
    };

    let pending = pending_permission_requests.remove_by_permission_id(&permission_id)?;
    if pending.method != ACP_PERMISSION_METHOD {
        return None;
    }

    Some((
        pending.id.clone(),
        normalize_permission_result(&payload, &pending),
    ))
}

pub(crate) fn is_cancel_method_not_found(response: &JsonRpcResponse) -> bool {
    let Some(error) = response.error() else {
        return false;
    };
    if error.code != -32601 {
        return false;
    }

    if let Some(data) = error.data.as_ref().and_then(Value::as_object) {
        if data
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| method == ACP_CANCEL_METHOD)
        {
            return true;
        }
    }

    error.message.contains(ACP_CANCEL_METHOD)
}

pub(crate) fn derive_config_options(
    agent_type: &str,
    session_result: &Map<String, Value>,
) -> Vec<Value> {
    let Some(models) = session_result.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    let current_model_id = models
        .get("currentModelId")
        .and_then(Value::as_str)
        .map(String::from);
    let allowed_values = models
        .get("availableModels")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(Value::as_object)
                .filter_map(|model| {
                    let model_id = model.get("modelId")?.as_str()?;
                    let mut item = Map::from_iter([(
                        String::from("id"),
                        Value::String(String::from(model_id)),
                    )]);
                    if let Some(name) = model.get("name").and_then(Value::as_str) {
                        item.insert(String::from("label"), Value::String(String::from(name)));
                    }
                    Some(Value::Object(item))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if current_model_id.is_none() && allowed_values.is_empty() {
        return Vec::new();
    }

    let mut option = Map::from_iter([
        (String::from("id"), Value::String(String::from("model"))),
        (
            String::from("category"),
            Value::String(String::from("model")),
        ),
        (String::from("label"), Value::String(String::from("Model"))),
        (String::from("allowedValues"), Value::Array(allowed_values)),
        (
            String::from("readOnly"),
            Value::Bool(matches!(
                compatibility_for(agent_type),
                AgentCompatibilityKind::OpenCode
            )),
        ),
    ]);
    if let Some(current_model_id) = current_model_id {
        option.insert(
            String::from("currentValue"),
            Value::String(current_model_id),
        );
    }
    if matches!(
        compatibility_for(agent_type),
        AgentCompatibilityKind::OpenCode
    ) {
        option.insert(
            String::from("description"),
            Value::String(String::from(
                "Available models reported by OpenCode. Model switching must be configured before opening the session because ACP session/set_config_option is not implemented.",
            )),
        );
    }

    vec![Value::Object(option)]
}

pub(crate) fn synthetic_mode_update(mode_id: &str) -> JsonRpcNotification {
    JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "current_mode_update",
                "currentModeId": mode_id,
            }
        })),
    }
}

pub(crate) fn synthetic_config_update(config_options: &[Value]) -> JsonRpcNotification {
    JsonRpcNotification {
        jsonrpc: String::from("2.0"),
        method: String::from("session/update"),
        params: Some(json!({
            "update": {
                "sessionUpdate": "config_option_update",
                "configOptions": config_options,
            }
        })),
    }
}

pub(crate) fn truncate_activity_text(value: &str) -> String {
    if value.len() <= ACTIVITY_TEXT_LIMIT {
        return String::from(value);
    }
    format!("{}...", &value[..ACTIVITY_TEXT_LIMIT])
}

pub(crate) fn summarize_inbound_notification(notification: &JsonRpcNotification) -> String {
    truncate_activity_text(&format!("received notification {}", notification.method))
}

pub(crate) fn summarize_inbound_request(request: &JsonRpcRequest) -> String {
    truncate_activity_text(&format!(
        "received request {} id={}",
        request.method, request.id
    ))
}

pub(crate) fn summarize_inbound_response(response: &JsonRpcResponse) -> String {
    match response.error() {
        Some(error) => truncate_activity_text(&format!(
            "received response id={} error={}:{}",
            response.id, error.code, error.message
        )),
        None => format!("received response id={}", response.id),
    }
}

fn normalize_permission_result(
    params: &Map<String, Value>,
    pending: &PendingPermissionRequest,
) -> Value {
    if let Some(outcome) = params.get("outcome") {
        if outcome.is_object() {
            return json!({ "outcome": outcome });
        }
    }

    let requested_reply = params.get("reply").and_then(Value::as_str);
    if let Some(selected_option_id) =
        resolve_permission_option_id(&pending.options, requested_reply)
    {
        return json!({
            "outcome": {
                "outcome": "selected",
                "optionId": selected_option_id,
            }
        });
    }

    match requested_reply {
        Some("always") => {
            json!({ "outcome": { "outcome": "selected", "optionId": "allow_always" } })
        }
        Some("once") => json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } }),
        Some("reject") => {
            json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } })
        }
        _ => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

fn resolve_permission_option_id(
    options: &Option<Vec<Map<String, Value>>>,
    reply: Option<&str>,
) -> Option<String> {
    let reply = reply?;
    let targets = match reply {
        "always" => (["always", "allow_always"], ["allow_always"]),
        "once" => (["once", "allow_once"], ["allow_once"]),
        "reject" => (["reject", "reject_once"], ["reject_once"]),
        _ => return None,
    };

    let options = options.as_ref()?;
    let matched = options.iter().find(|option| {
        let option_id_matches = option
            .get("optionId")
            .and_then(Value::as_str)
            .map(|value| targets.0.contains(&value))
            .unwrap_or(false);
        let kind_matches = option
            .get("kind")
            .and_then(Value::as_str)
            .map(|value| targets.1.contains(&value))
            .unwrap_or(false);
        option_id_matches || kind_matches
    })?;

    matched
        .get("optionId")
        .and_then(Value::as_str)
        .map(String::from)
}

pub(crate) fn to_record(value: Option<Value>) -> Map<String, Value> {
    match value {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seen_inbound_request_ids_evict_oldest_entry_after_retention_window() {
        let mut seen = SeenInboundRequestIds::new(2);
        let first = JsonRpcId::Number(1);
        let second = JsonRpcId::Number(2);
        let third = JsonRpcId::Number(3);

        seen.insert(first.clone());
        seen.insert(second.clone());
        assert!(seen.contains(&first));
        assert!(seen.contains(&second));

        seen.insert(third.clone());
        assert_eq!(seen.len(), 2);
        assert!(!seen.contains(&first));
        assert!(seen.contains(&second));
        assert!(seen.contains(&third));
    }

    #[test]
    fn permission_requests_evict_pending_entries_with_seen_request_window() {
        let mut seen = SeenInboundRequestIds::new(2);
        let mut pending = PendingPermissionRequests::new(2);

        for request_id in 1..=3 {
            let request = JsonRpcRequest {
                jsonrpc: String::from("2.0"),
                id: JsonRpcId::Number(request_id),
                method: String::from("session/request_permission"),
                params: Some(json!({ "path": format!("/tmp/{request_id}.txt") })),
            };
            let notification =
                normalize_inbound_permission_request(&request, &mut seen, &mut pending)
                    .expect("permission request should normalize");
            assert_eq!(notification.method, LEGACY_PERMISSION_METHOD);
        }

        assert_eq!(seen.len(), 2);
        assert_eq!(pending.len(), 2);
        assert!(!pending.contains_id(&JsonRpcId::Number(1)));
        assert!(pending.contains_id(&JsonRpcId::Number(2)));
        assert!(pending.contains_id(&JsonRpcId::Number(3)));
    }

    #[test]
    fn pending_permission_eviction_uses_typed_json_rpc_ids() {
        let mut pending = PendingPermissionRequests::new(2);

        for id in [
            JsonRpcId::Number(1),
            JsonRpcId::String(String::from("1")),
            JsonRpcId::Number(2),
        ] {
            pending.insert(PendingPermissionRequest {
                id,
                method: String::from(ACP_PERMISSION_METHOD),
                options: None,
            });
        }

        assert_eq!(pending.len(), 2);
        assert!(!pending.contains_id(&JsonRpcId::Number(1)));
        assert!(pending.contains_id(&JsonRpcId::String(String::from("1"))));
        assert!(pending.contains_id(&JsonRpcId::Number(2)));
    }

    #[test]
    fn permission_ids_are_collision_safe_for_string_and_number_ids() {
        let mut seen = SeenInboundRequestIds::new(4);
        let mut pending = PendingPermissionRequests::new(4);

        let number_request = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::Number(1),
            method: String::from("session/request_permission"),
            params: Some(json!({ "path": "/tmp/number.txt" })),
        };
        let string_request = JsonRpcRequest {
            jsonrpc: String::from("2.0"),
            id: JsonRpcId::String(String::from("1")),
            method: String::from("session/request_permission"),
            params: Some(json!({ "path": "/tmp/string.txt" })),
        };

        let number_notification =
            normalize_inbound_permission_request(&number_request, &mut seen, &mut pending)
                .expect("number permission request should normalize");
        let string_notification =
            normalize_inbound_permission_request(&string_request, &mut seen, &mut pending)
                .expect("string permission request should normalize");

        let number_permission_id = number_notification
            .params
            .as_ref()
            .and_then(|params| params.get("permissionId"))
            .and_then(Value::as_str)
            .expect("number permission id");
        let string_permission_id = string_notification
            .params
            .as_ref()
            .and_then(|params| params.get("permissionId"))
            .and_then(Value::as_str)
            .expect("string permission id");
        assert_eq!(number_permission_id, "1");
        assert_ne!(string_permission_id, "1");

        let (string_reply_id, _) = maybe_normalize_permission_response(
            LEGACY_PERMISSION_METHOD,
            Some(json!({
                "permissionId": string_permission_id,
                "reply": "reject",
            })),
            &mut pending,
        )
        .expect("string permission reply should resolve");
        assert_eq!(string_reply_id, JsonRpcId::String(String::from("1")));

        let (number_reply_id, _) = maybe_normalize_permission_response(
            LEGACY_PERMISSION_METHOD,
            Some(json!({
                "permissionId": number_permission_id,
                "reply": "reject",
            })),
            &mut pending,
        )
        .expect("number permission reply should resolve");
        assert_eq!(number_reply_id, JsonRpcId::Number(1));
    }
}
