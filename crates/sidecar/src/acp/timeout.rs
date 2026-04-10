use crate::acp::JsonRpcId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpTimeoutDiagnostics {
    pub(crate) kind: String,
    pub(crate) method: String,
    pub(crate) id: JsonRpcId,
    pub(crate) timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) killed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) transport_state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) recent_activity: Vec<String>,
}

impl AcpTimeoutDiagnostics {
    pub(crate) fn new(
        method: impl Into<String>,
        id: JsonRpcId,
        timeout_ms: u64,
        exit_code: Option<i32>,
        killed: Option<bool>,
        transport_state: Option<String>,
        recent_activity: Vec<String>,
    ) -> Self {
        Self {
            kind: String::from("acp_timeout"),
            method: method.into(),
            id,
            timeout_ms,
            exit_code,
            killed,
            transport_state,
            recent_activity,
        }
    }

    pub(crate) fn message(&self) -> String {
        let transport_state = self
            .transport_state
            .as_ref()
            .map(|state| format!("{state}. "))
            .unwrap_or_default();
        let exit_code = self
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| String::from("unknown"));
        let killed = self
            .killed
            .map(|value| format!(" killed={value}."))
            .unwrap_or_default();
        let activity = if self.recent_activity.is_empty() {
            String::from("no recent ACP activity")
        } else {
            self.recent_activity.join(" | ")
        };
        format!(
            "ACP request {} (id={}) timed out after {}ms. {}process exitCode={exit_code}.{killed} Recent ACP activity: {activity}",
            self.method, self.id, self.timeout_ms, transport_state
        )
    }

    pub(crate) fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("serialize ACP timeout diagnostics")
    }
}
