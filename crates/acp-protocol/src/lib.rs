#![forbid(unsafe_code)]

//! Agent OS ACP extension protocol types.

pub mod generated;

pub const ACP_EXTENSION_NAMESPACE: &str = "dev.rivet.agent-os.acp";
pub const PROTOCOL_NAME: &str = "agentos-acp";
pub const PROTOCOL_VERSION: u16 = 1;
