use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::fmt;

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

#[derive(Debug, Clone, PartialEq)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: JsonRpcId,
    result: Option<Value>,
    error: Option<JsonRpcError>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcResponseShapeError {
    BothResultAndError,
    MissingResultAndError,
}

impl fmt::Display for JsonRpcResponseShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BothResultAndError => {
                f.write_str("JSON-RPC response cannot include both result and error")
            }
            Self::MissingResultAndError => {
                f.write_str("JSON-RPC response must include exactly one of result or error")
            }
        }
    }
}

impl std::error::Error for JsonRpcResponseShapeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonRpcParseErrorKind {
    ParseError,
    InvalidRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcParseError {
    kind: JsonRpcParseErrorKind,
    id: JsonRpcId,
    message: String,
}

impl JsonRpcParseError {
    fn parse_error(error: serde_json::Error) -> Self {
        Self {
            kind: JsonRpcParseErrorKind::ParseError,
            id: JsonRpcId::Null,
            message: format!("Parse error: {error}"),
        }
    }

    fn invalid_request(message: impl Into<String>, id: Option<JsonRpcId>) -> Self {
        Self {
            kind: JsonRpcParseErrorKind::InvalidRequest,
            id: id.unwrap_or(JsonRpcId::Null),
            message: message.into(),
        }
    }

    pub fn code(&self) -> i64 {
        match self.kind {
            JsonRpcParseErrorKind::ParseError => -32700,
            JsonRpcParseErrorKind::InvalidRequest => -32600,
        }
    }

    pub fn id(&self) -> &JsonRpcId {
        &self.id
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn to_response(&self) -> JsonRpcResponse {
        JsonRpcResponse::error_response(
            self.id.clone(),
            JsonRpcError {
                code: self.code(),
                message: self.message.clone(),
                data: None,
            },
        )
    }
}

impl fmt::Display for JsonRpcParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code())
    }
}

impl std::error::Error for JsonRpcParseError {}

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

impl JsonRpcResponse {
    pub fn success(id: JsonRpcId, result: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error_response(id: JsonRpcId, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn try_from_parts(
        jsonrpc: String,
        id: JsonRpcId,
        result: Option<Value>,
        error: Option<JsonRpcError>,
    ) -> Result<Self, JsonRpcResponseShapeError> {
        match (result, error) {
            (Some(result), None) => Ok(Self {
                jsonrpc,
                id,
                result: Some(result),
                error: None,
            }),
            (None, Some(error)) => Ok(Self {
                jsonrpc,
                id,
                result: None,
                error: Some(error),
            }),
            (Some(_), Some(_)) => Err(JsonRpcResponseShapeError::BothResultAndError),
            (None, None) => Err(JsonRpcResponseShapeError::MissingResultAndError),
        }
    }

    pub fn result(&self) -> Option<&Value> {
        self.result.as_ref()
    }

    pub fn error(&self) -> Option<&JsonRpcError> {
        self.error.as_ref()
    }

    pub fn into_result(self) -> Option<Value> {
        self.result
    }

    pub fn into_error(self) -> Option<JsonRpcError> {
        self.error
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
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

pub fn deserialize_message(line: &str) -> Result<JsonRpcMessage, JsonRpcParseError> {
    let value: Value = serde_json::from_str(line).map_err(JsonRpcParseError::parse_error)?;
    let object = value.as_object().ok_or_else(|| {
        JsonRpcParseError::invalid_request(
            "Invalid Request: JSON-RPC payload must be an object",
            None,
        )
    })?;
    parse_message_object(object)
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

impl Serialize for JsonRpcResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = Map::new();
        map.insert(String::from("jsonrpc"), Value::String(self.jsonrpc.clone()));
        map.insert(
            String::from("id"),
            serde_json::to_value(&self.id).map_err(S::Error::custom)?,
        );
        if let Some(result) = &self.result {
            map.insert(String::from("result"), result.clone());
        } else if let Some(error) = &self.error {
            map.insert(
                String::from("error"),
                serde_json::to_value(error).map_err(S::Error::custom)?,
            );
        }
        map.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonRpcResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawJsonRpcResponse {
            #[serde(default = "jsonrpc_version")]
            jsonrpc: String,
            id: JsonRpcId,
            result: Option<Value>,
            error: Option<JsonRpcError>,
        }

        let raw = RawJsonRpcResponse::deserialize(deserializer)?;
        JsonRpcResponse::try_from_parts(raw.jsonrpc, raw.id, raw.result, raw.error)
            .map_err(D::Error::custom)
    }
}

fn parse_message_object(object: &Map<String, Value>) -> Result<JsonRpcMessage, JsonRpcParseError> {
    validate_jsonrpc_version(object)?;

    if object.contains_key("method") {
        return parse_request_or_notification(object);
    }

    if object.contains_key("result") || object.contains_key("error") || object.contains_key("id") {
        return parse_response(object);
    }

    Err(JsonRpcParseError::invalid_request(
        "Invalid Request: missing method/result/error",
        parsed_id(object.get("id")),
    ))
}

fn validate_jsonrpc_version(object: &Map<String, Value>) -> Result<(), JsonRpcParseError> {
    let id = parsed_id(object.get("id"));
    match object.get("jsonrpc").and_then(Value::as_str) {
        Some(JSON_RPC_VERSION) => Ok(()),
        Some(_) => Err(JsonRpcParseError::invalid_request(
            "Invalid Request: jsonrpc must be \"2.0\"",
            id,
        )),
        None => Err(JsonRpcParseError::invalid_request(
            "Invalid Request: missing jsonrpc version",
            id,
        )),
    }
}

fn parse_request_or_notification(
    object: &Map<String, Value>,
) -> Result<JsonRpcMessage, JsonRpcParseError> {
    let method = object
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            JsonRpcParseError::invalid_request(
                "Invalid Request: method must be a string",
                parsed_id(object.get("id")),
            )
        })?;
    validate_params_shape(object)?;

    let params = object.get("params").cloned();
    if let Some(id) = parsed_required_id(object)? {
        return Ok(JsonRpcMessage::Request(JsonRpcRequest {
            jsonrpc: jsonrpc_version(),
            id,
            method: String::from(method),
            params,
        }));
    }

    Ok(JsonRpcMessage::Notification(JsonRpcNotification {
        jsonrpc: jsonrpc_version(),
        method: String::from(method),
        params,
    }))
}

fn parse_response(object: &Map<String, Value>) -> Result<JsonRpcMessage, JsonRpcParseError> {
    let id = parsed_required_id(object)?.ok_or_else(|| {
        JsonRpcParseError::invalid_request("Invalid Request: response is missing id", None)
    })?;
    let result = object.get("result").cloned();
    let error = match object.get("error") {
        Some(value) => Some(
            serde_json::from_value::<JsonRpcError>(value.clone()).map_err(|error| {
                JsonRpcParseError::invalid_request(
                    format!("Invalid Request: malformed error payload: {error}"),
                    Some(id.clone()),
                )
            })?,
        ),
        None => None,
    };

    let response = JsonRpcResponse::try_from_parts(jsonrpc_version(), id.clone(), result, error)
        .map_err(|error| {
            JsonRpcParseError::invalid_request(format!("Invalid Request: {error}"), Some(id))
        })?;
    Ok(JsonRpcMessage::Response(response))
}

fn validate_params_shape(object: &Map<String, Value>) -> Result<(), JsonRpcParseError> {
    let Some(params) = object.get("params") else {
        return Ok(());
    };
    if params.is_array() || params.is_object() {
        return Ok(());
    }

    Err(JsonRpcParseError::invalid_request(
        "Invalid Request: params must be an object or array",
        parsed_id(object.get("id")),
    ))
}

fn parsed_required_id(value: &Map<String, Value>) -> Result<Option<JsonRpcId>, JsonRpcParseError> {
    match value.get("id") {
        Some(value) => parsed_id(Some(value))
            .ok_or_else(|| {
                JsonRpcParseError::invalid_request(
                    "Invalid Request: id must be a string, number, or null",
                    None,
                )
            })
            .map(Some),
        None => Ok(None),
    }
}

fn parsed_id(value: Option<&Value>) -> Option<JsonRpcId> {
    match value {
        Some(Value::String(value)) => Some(JsonRpcId::String(value.clone())),
        Some(Value::Number(value)) => value.as_i64().map(JsonRpcId::Number),
        Some(Value::Null) => Some(JsonRpcId::Null),
        _ => None,
    }
}
