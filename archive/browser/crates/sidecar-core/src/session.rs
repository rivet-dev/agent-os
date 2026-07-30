//! Host-free per-session data model for the ACP extension.
//!
//! Lifted verbatim (host-free) from the native `agentos-sidecar::acp_extension`
//! so both backends share the same record shape. No tokio/host types here; the
//! native sidecar wraps a `BTreeMap<String, AcpSessionRecord>` in its own lock and
//! the browser sidecar (single-threaded) holds it directly.

use agentos_protocol::generated::v1::{AcpSessionCreatedResponse, AcpSessionStateResponse};

/// State the sidecar tracks for one live ACP session.
#[derive(Debug, Clone)]
pub struct AcpSessionRecord {
    pub session_id: String,
    /// Connection that created this session. Enforces per-connection ownership so
    /// one connection cannot read or drive another connection's ACP session by id.
    pub owner_connection_id: String,
    pub agent_type: String,
    pub process_id: String,
    pub pid: Option<u32>,
    pub modes: Option<String>,
    pub config_options: Vec<String>,
    pub agent_capabilities: Option<String>,
    pub agent_info: Option<String>,
    pub stdout_buffer: String,
    pub next_request_id: i64,
    pub closed: bool,
    pub exit_code: Option<i32>,
    /// Set by the resume fallback tier; the transcript-continuation preamble is
    /// prepended once to this session's next `session/prompt`, then cleared.
    pub pending_preamble: Option<String>,
}

impl AcpSessionRecord {
    /// Allocate the next JSON-RPC request id for this session.
    pub fn allocate_request_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Build the `session/created` response from this record.
    pub fn created_response(&self) -> AcpSessionCreatedResponse {
        AcpSessionCreatedResponse {
            session_id: self.session_id.clone(),
            pid: self.pid,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
        }
    }

    /// Build the `session/state` response from this record.
    pub fn state_response(&self) -> AcpSessionStateResponse {
        AcpSessionStateResponse {
            session_id: self.session_id.clone(),
            agent_type: self.agent_type.clone(),
            process_id: self.process_id.clone(),
            pid: self.pid,
            closed: self.closed,
            exit_code: self.exit_code,
            modes: self.modes.clone(),
            config_options: self.config_options.clone(),
            agent_capabilities: self.agent_capabilities.clone(),
            agent_info: self.agent_info.clone(),
        }
    }
}
