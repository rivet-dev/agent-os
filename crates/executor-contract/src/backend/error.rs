use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::error::Error;
use std::fmt;

/// Stable error crossing the kernel/host-service/adapter boundary.
///
/// `code` is a Linux errno name or an AgentOS typed limit/runtime code. Engine
/// error strings are diagnostics only and must never be parsed to recover it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostServiceError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl HostServiceError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn limit(
        code: impl Into<String>,
        limit_name: &'static str,
        limit: u64,
        observed: u64,
    ) -> Self {
        let code = code.into();
        Self::new(
            code,
            format!(
                "{limit_name} limit is {limit}, observed {observed}; raise {limit_name} if needed"
            ),
        )
        .with_details(serde_json::json!({
            "limitName": limit_name,
            "configPath": limit_name,
            "limit": limit,
            "observed": observed,
        }))
    }
}

impl fmt::Display for HostServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Error for HostServiceError {}
