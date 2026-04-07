use crate::acp::json_rpc::{JsonRpcId, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const LEGACY_PERMISSION_METHOD: &str = "request/permission";
pub(crate) const ACP_PERMISSION_METHOD: &str = "session/request_permission";
pub(crate) const ACP_CANCEL_METHOD: &str = "session/cancel";
pub(crate) const RECENT_ACTIVITY_LIMIT: usize = 20;
pub(crate) const ACTIVITY_TEXT_LIMIT: usize = 240;

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

pub(crate) fn compatibility_for(agent_type: &str) -> AgentCompatibilityKind {
    match agent_type {
        "opencode" => AgentCompatibilityKind::OpenCode,
        _ => AgentCompatibilityKind::Generic,
    }
}

pub(crate) fn normalize_inbound_permission_request(
    request: &JsonRpcRequest,
    seen_inbound_request_ids: &mut BTreeSet<JsonRpcId>,
    pending_permission_requests: &mut BTreeMap<String, PendingPermissionRequest>,
) -> Option<JsonRpcNotification> {
    if request.method != ACP_PERMISSION_METHOD {
        return None;
    }

    if seen_inbound_request_ids.contains(&request.id) {
        return None;
    }
    seen_inbound_request_ids.insert(request.id.clone());

    let params = to_record(request.params.clone());
    let permission_id = request.id.to_string();
    pending_permission_requests.insert(
        permission_id.clone(),
        PendingPermissionRequest {
            id: request.id.clone(),
            method: request.method.clone(),
            options: params.get("options").and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_object)
                    .cloned()
                    .collect::<Vec<_>>()
            }),
        },
    );

    let mut normalized = params;
    normalized.insert(
        String::from("permissionId"),
        Value::String(permission_id),
    );
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
    pending_permission_requests: &mut BTreeMap<String, PendingPermissionRequest>,
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

    let pending = pending_permission_requests.remove(&permission_id)?;
    if pending.method != ACP_PERMISSION_METHOD {
        return None;
    }

    Some((pending.id.clone(), normalize_permission_result(&payload, &pending)))
}

pub(crate) fn is_cancel_method_not_found(response: &JsonRpcResponse) -> bool {
    let Some(error) = &response.error else {
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
        (
            String::from("allowedValues"),
            Value::Array(allowed_values),
        ),
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
                "Available models reported by OpenCode. Model switching must be configured before createSession() because ACP session/set_config_option is not implemented.",
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
    match &response.error {
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
    if let Some(selected_option_id) = resolve_permission_option_id(&pending.options, requested_reply)
    {
        return json!({
            "outcome": {
                "outcome": "selected",
                "optionId": selected_option_id,
            }
        });
    }

    match requested_reply {
        Some("always") => json!({ "outcome": { "outcome": "selected", "optionId": "allow_always" } }),
        Some("once") => json!({ "outcome": { "outcome": "selected", "optionId": "allow_once" } }),
        Some("reject") => json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } }),
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
