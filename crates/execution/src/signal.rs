use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSignalDispositionAction {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSignalHandlerRegistration {
    pub action: ExecutionSignalDispositionAction,
    pub mask: Vec<u32>,
    pub flags: u32,
}
