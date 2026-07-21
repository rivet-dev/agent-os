#![forbid(unsafe_code)]

//! Low-level Rust client transport for the AgentOS language execution native sidecar.
//!
//! This crate owns the framed stdio transport and exposes the generated AgentOS language execution wire protocol.
//! Higher level products layer their own authentication, extension payloads, and
//! typed API surfaces on top of this transport.

pub mod error;
pub mod transport;
pub mod wire;

pub use error::{ProtocolCodecError, TransportError, TransportResult};
pub use transport::{SidecarTransport, WireSidecarCallback};
