//! Synchronous host-operation seam for the ACP core.
//!
//! The ACP orchestration (create/resume session, session/prompt, etc.) needs to
//! spawn the agent process, drive its stdin/stdout, and touch the guest fs. Those
//! are the only host-coupled operations; everything else (JSON-RPC framing, session
//! state, the ACP protocol) is pure logic that belongs in this host-free crate.
//!
//! This trait is the seam: it is SYNCHRONOUS (the browser sidecar runs on a single
//! thread behind the SharedArrayBuffer sync bridge, and the native sidecar adapts
//! its async runtime to satisfy it). Porting the async `acp_extension` orchestration
//! onto this trait — so the same state machine runs on both backends — is the
//! remaining migration; see `AGENTOS-WEB-CONVERGENCE.md`.

use std::collections::BTreeMap;

use agentos_protocol::generated::v1::AcpRuntimeKind;

use crate::AcpCoreError;

/// Request to launch an agent adapter process inside the VM.
#[derive(Debug, Clone)]
pub struct SpawnAgentRequest {
    pub process_id: String,
    pub runtime: AcpRuntimeKind,
    pub entrypoint: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

/// Result of spawning an agent adapter process.
#[derive(Debug, Clone)]
pub struct SpawnedAgent {
    pub process_id: String,
    pub pid: Option<u32>,
}

/// A single piece of output drained from a running agent process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentOutput {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited(Option<i32>),
}

/// Synchronous host operations the ACP core drives. Implemented by the native
/// sidecar (adapting its async runtime) and the browser sidecar (direct, single
/// threaded). Every operation routes through the kernel, the sole enforcement
/// point; this seam never bypasses an applied permission/limit.
pub trait AcpHost {
    fn spawn_agent(&mut self, request: SpawnAgentRequest) -> Result<SpawnedAgent, AcpCoreError>;
    /// Associate a spawned process with its ACP session id (for routing/teardown).
    fn bind_session(&mut self, session_id: &str, process_id: &str) -> Result<(), AcpCoreError>;
    fn write_stdin(&mut self, process_id: &str, chunk: &[u8]) -> Result<(), AcpCoreError>;
    fn close_stdin(&mut self, process_id: &str) -> Result<(), AcpCoreError>;
    /// Drain the next available output event, or `None` if nothing is pending.
    fn poll_output(&mut self, process_id: &str) -> Result<Option<AgentOutput>, AcpCoreError>;
    fn kill_agent(&mut self, process_id: &str, signal: &str) -> Result<(), AcpCoreError>;
    /// Block (up to `timeout_ms`) for the process to exit. Returns `Some(exit_code)`
    /// if it exited within the window, `None` on timeout. Native: a runtime timeout;
    /// browser: a bounded poll over the sync bridge.
    fn wait_for_exit(
        &mut self,
        process_id: &str,
        timeout_ms: u64,
    ) -> Result<Option<i32>, AcpCoreError>;
    fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), AcpCoreError>;
    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, AcpCoreError>;
    /// Monotonic-ish milliseconds, used for JSON-RPC timeouts. Native: Instant;
    /// browser: performance.now() via the host bridge.
    fn now_ms(&self) -> u64;
}
