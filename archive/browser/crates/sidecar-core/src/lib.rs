#![forbid(unsafe_code)]

//! Host-free core for the Agent OS ACP sidecar extension.
//!
//! This crate holds the parts of the ACP extension that do NOT depend on the host
//! runtime (tokio, std::fs, the native agentos sidecar): the request/response
//! wire codec and the per-session data model. It compiles to wasm32 so the browser
//! sidecar (`agentos-sidecar-browser`) can run the same ACP logic the native sidecar
//! (`agentos-sidecar`) runs, with each backend supplying the host operations
//! (process spawn, stdin write, output poll, kill) through a thin seam.
//!
//! Porting status: the codec + session model are host-free here. The async ACP
//! orchestration in `agentos-sidecar::acp_extension` is being migrated onto a
//! synchronous host seam defined here (see `AcpHost`); until that migration lands,
//! the native sidecar keeps its own async implementation.

use std::fmt;

use agentos_protocol::generated::v1::{AcpErrorResponse, AcpResponse};

pub mod codec;
pub mod engine;
pub mod host;
pub mod json_rpc;
pub mod session;

pub use engine::{AcpCore, ResumeStep};
pub use host::AcpHost;
pub use session::AcpSessionRecord;

/// Host-free error type for the ACP core. Mirrors the wire `AcpErrorResponse`
/// shape (a stable string `code` + human message) so it round-trips identically
/// to the native sidecar's `SidecarError`-derived responses without depending on
/// the native error enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcpCoreError {
    InvalidState(String),
    Unauthorized(String),
    Unsupported(String),
    Conflict(String),
    Execution(String),
}

impl AcpCoreError {
    /// Stable machine code, matching the native sidecar's `error_code` mapping so
    /// clients see identical codes regardless of backend.
    pub fn code(&self) -> &'static str {
        match self {
            AcpCoreError::InvalidState(_) => "invalid_state",
            AcpCoreError::Unauthorized(_) => "unauthorized",
            AcpCoreError::Unsupported(_) => "unsupported",
            AcpCoreError::Conflict(_) => "conflict",
            AcpCoreError::Execution(_) => "execution",
        }
    }
}

impl fmt::Display for AcpCoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AcpCoreError::InvalidState(message)
            | AcpCoreError::Unauthorized(message)
            | AcpCoreError::Unsupported(message)
            | AcpCoreError::Conflict(message)
            | AcpCoreError::Execution(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for AcpCoreError {}

/// The ACP extension namespace, re-exported for convenience.
pub use agentos_protocol::ACP_EXTENSION_NAMESPACE;

/// Build the wire error response for an ACP core error.
pub fn error_response(error: &AcpCoreError) -> AcpResponse {
    AcpResponse::AcpErrorResponse(AcpErrorResponse {
        code: error.code().to_string(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            AcpCoreError::InvalidState("x".into()).code(),
            "invalid_state"
        );
        assert_eq!(AcpCoreError::Unsupported("x".into()).code(), "unsupported");
        assert_eq!(
            error_response(&AcpCoreError::Conflict("dup".into())),
            AcpResponse::AcpErrorResponse(AcpErrorResponse {
                code: "conflict".into(),
                message: "dup".into(),
            })
        );
    }
}
