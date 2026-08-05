#![forbid(unsafe_code)]
// Wire enums intentionally have wide size variance (small acks next to bulky
// payload variants); boxing them is a wire-adjacent refactor tracked separately.
#![allow(clippy::large_enum_variant, clippy::result_large_err)]

//! Shared agentOS language execution sidecar wire protocol surface.

pub mod config;
pub mod generated_protocol;
pub mod protocol;
pub mod wire;

pub use config::SidecarProtocolConfig;
