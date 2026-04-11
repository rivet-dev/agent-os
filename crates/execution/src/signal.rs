use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeSignalDispositionAction {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSignalHandlerRegistration {
    pub action: NodeSignalDispositionAction,
    pub mask: Vec<u32>,
    pub flags: u32,
}
