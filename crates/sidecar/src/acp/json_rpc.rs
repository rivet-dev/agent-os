use serde::{Deserialize, Serialize};
use serde_json::Value;

const JSON_RPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcId {
    Number(i64),
    String(String),
    Null,
}

impl std::fmt::Display for JsonRpcId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => f.write_str(value),
            Self::Null => f.write_str("null"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub id: JsonRpcId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Response(JsonRpcResponse),
    Notification(JsonRpcNotification),
}

impl From<JsonRpcRequest> for JsonRpcMessage {
    fn from(value: JsonRpcRequest) -> Self {
        Self::Request(value)
    }
}

impl From<JsonRpcResponse> for JsonRpcMessage {
    fn from(value: JsonRpcResponse) -> Self {
        Self::Response(value)
    }
}

impl From<JsonRpcNotification> for JsonRpcMessage {
    fn from(value: JsonRpcNotification) -> Self {
        Self::Notification(value)
    }
}

pub fn serialize_message(message: &JsonRpcMessage) -> Result<String, serde_json::Error> {
    let body = match message {
        JsonRpcMessage::Request(value) => serde_json::to_string(value)?,
        JsonRpcMessage::Response(value) => serde_json::to_string(value)?,
        JsonRpcMessage::Notification(value) => serde_json::to_string(value)?,
    };
    Ok(format!("{body}\n"))
}

pub fn deserialize_message(line: &str) -> Option<JsonRpcMessage> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("jsonrpc")?.as_str()? != JSON_RPC_VERSION {
        return None;
    }

    if value.get("method").is_some() {
        if value.get("id").is_some() {
            return serde_json::from_value::<JsonRpcRequest>(value)
                .ok()
                .map(JsonRpcMessage::Request);
        }
        return serde_json::from_value::<JsonRpcNotification>(value)
            .ok()
            .map(JsonRpcMessage::Notification);
    }

    if value.get("id").is_some() {
        return serde_json::from_value::<JsonRpcResponse>(value)
            .ok()
            .map(JsonRpcMessage::Response);
    }

    None
}

pub fn is_response(message: &JsonRpcMessage) -> bool {
    matches!(message, JsonRpcMessage::Response(_))
}

pub fn is_request(message: &JsonRpcMessage) -> bool {
    matches!(message, JsonRpcMessage::Request(_))
}

fn jsonrpc_version() -> String {
    String::from(JSON_RPC_VERSION)
}
