use crate::ipc_binary::{BinaryFrame, ExecutionErrorBin};
use agentos_runtime::readiness::ReadyFlags;
use std::io;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmSessionHint {
    pub bridge_code: String,
    pub userland_code: String,
    pub heap_limit_mb: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCommand {
    CreateSession {
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
        wall_clock_limit_ms: Option<u32>,
        warm_hint: Option<WarmSessionHint>,
    },
    DestroySession {
        session_id: String,
    },
    PauseSession {
        session_id: String,
    },
    ResumeSession {
        session_id: String,
    },
    WarmSnapshot {
        bridge_code: String,
        userland_code: String,
    },
    SendToSession {
        session_id: String,
        message: SessionMessage,
    },
    /// In-process capability readiness publication. Durable data stays in the
    /// owning sidecar subsystem; this command carries identity and level state.
    PublishReadiness {
        session_id: String,
        capability_id: u64,
        capability_generation: u64,
        flags: ReadyFlags,
    },
    RemoveReadiness {
        session_id: String,
        capability_id: u64,
        capability_generation: u64,
    },
    PublishSignal {
        session_id: String,
        signal: i32,
    },
    PublishTimer {
        session_id: String,
        timer_id: u64,
    },
    /// Install a direct module-source reader on a session thread. Carries the live
    /// reader (not serialized) so module loads skip the bridge round-trip. Routed
    /// through the dispatch thread (which owns the session manager) like
    /// `SendToSession`, then forwarded as a `SessionCommand::SetModuleReader`.
    SetSessionModuleReader {
        session_id: String,
        reader: ModuleReaderHandle,
    },
}

/// Wrapper that lets a live `GuestModuleReader` ride `RuntimeCommand` despite its
/// `derive(Debug, Clone, PartialEq)`: the trait object lives behind an
/// `Arc<Mutex<Option<..>>>`, and the dispatch thread `take()`s it out exactly once.
#[derive(Clone)]
pub struct ModuleReaderHandle(
    std::sync::Arc<std::sync::Mutex<Option<Box<dyn crate::execution::GuestModuleReader>>>>,
);

impl ModuleReaderHandle {
    pub fn new(reader: Box<dyn crate::execution::GuestModuleReader>) -> Self {
        ModuleReaderHandle(std::sync::Arc::new(std::sync::Mutex::new(Some(reader))))
    }

    /// Take the reader out (dispatch-thread side); `None` if already taken.
    pub fn take(&self) -> Option<Box<dyn crate::execution::GuestModuleReader>> {
        self.0.lock().ok().and_then(|mut guard| guard.take())
    }
}

impl std::fmt::Debug for ModuleReaderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ModuleReaderHandle(..)")
    }
}

impl PartialEq for ModuleReaderHandle {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SessionMessage {
    InjectGlobals {
        payload: Vec<u8>,
    },
    Execute {
        mode: u8,
        file_path: String,
        bridge_code: String,
        post_restore_script: String,
        userland_code: String,
        high_resolution_time: bool,
        user_code: String,
        wasm_module_bytes: Option<Arc<Vec<u8>>>,
    },
    BridgeResponse(BridgeResponse),
    StreamEvent(StreamEvent),
    TerminateExecution,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BridgeResponse {
    pub call_id: u64,
    pub status: u8,
    pub payload: Vec<u8>,
    /// In-process byte ownership. IPC-created responses leave this empty;
    /// sidecar-owned direct responses retain it until V8 copies the payload.
    pub reservation: Option<agentos_runtime::accounting::SharedReservation>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamEvent {
    pub event_type: String,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeEvent {
    BridgeCall {
        session_id: String,
        call_id: u64,
        method: String,
        payload: Vec<u8>,
    },
    ExecutionResult {
        session_id: String,
        exit_code: i32,
        exports: Option<Vec<u8>>,
        error: Option<ExecutionErrorBin>,
    },
    Log {
        session_id: String,
        channel: u8,
        message: String,
    },
    StreamCallback {
        session_id: String,
        callback_type: String,
        payload: Vec<u8>,
    },
}

impl RuntimeEvent {
    pub fn session_id(&self) -> &str {
        match self {
            RuntimeEvent::BridgeCall { session_id, .. }
            | RuntimeEvent::ExecutionResult { session_id, .. }
            | RuntimeEvent::Log { session_id, .. }
            | RuntimeEvent::StreamCallback { session_id, .. } => session_id,
        }
    }
}

impl TryFrom<BinaryFrame> for RuntimeCommand {
    type Error = io::Error;

    fn try_from(frame: BinaryFrame) -> Result<Self, Self::Error> {
        match frame {
            BinaryFrame::CreateSession {
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
                wall_clock_limit_ms,
            } => Ok(RuntimeCommand::CreateSession {
                session_id,
                heap_limit_mb: non_zero_option(heap_limit_mb),
                cpu_time_limit_ms: non_zero_option(cpu_time_limit_ms),
                wall_clock_limit_ms: non_zero_option(wall_clock_limit_ms),
                warm_hint: None,
            }),
            BinaryFrame::DestroySession { session_id } => {
                Ok(RuntimeCommand::DestroySession { session_id })
            }
            BinaryFrame::InjectGlobals {
                session_id,
                payload,
            } => Ok(RuntimeCommand::SendToSession {
                session_id,
                message: SessionMessage::InjectGlobals { payload },
            }),
            BinaryFrame::Execute {
                session_id,
                mode,
                file_path,
                bridge_code,
                post_restore_script,
                userland_code,
                high_resolution_time,
                user_code,
            } => {
                if mode > 3 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("unknown Execute mode: {mode}"),
                    ));
                }
                Ok(RuntimeCommand::SendToSession {
                    session_id,
                    message: SessionMessage::Execute {
                        mode,
                        file_path,
                        bridge_code,
                        post_restore_script,
                        userland_code,
                        high_resolution_time,
                        user_code,
                        wasm_module_bytes: None,
                    },
                })
            }
            BinaryFrame::BridgeResponse {
                session_id,
                call_id,
                status,
                payload,
            } => {
                validate_bridge_response_status(status)?;
                Ok(RuntimeCommand::SendToSession {
                    session_id,
                    message: SessionMessage::BridgeResponse(BridgeResponse {
                        call_id,
                        status,
                        payload,
                        reservation: None,
                    }),
                })
            }
            BinaryFrame::StreamEvent {
                session_id,
                event_type,
                payload,
            } => Ok(RuntimeCommand::SendToSession {
                session_id,
                message: SessionMessage::StreamEvent(StreamEvent {
                    event_type,
                    payload,
                }),
            }),
            BinaryFrame::TerminateExecution { session_id } => Ok(RuntimeCommand::SendToSession {
                session_id,
                message: SessionMessage::TerminateExecution,
            }),
            BinaryFrame::WarmSnapshot {
                bridge_code,
                userland_code,
            } => Ok(RuntimeCommand::WarmSnapshot {
                bridge_code,
                userland_code,
            }),
            BinaryFrame::Authenticate { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Authenticate is not supported by the embedded runtime",
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "host-output frames cannot be sent into the embedded runtime",
            )),
        }
    }
}

impl From<RuntimeEvent> for BinaryFrame {
    fn from(event: RuntimeEvent) -> Self {
        match event {
            RuntimeEvent::BridgeCall {
                session_id,
                call_id,
                method,
                payload,
            } => BinaryFrame::BridgeCall {
                session_id,
                call_id,
                method,
                payload,
            },
            RuntimeEvent::ExecutionResult {
                session_id,
                exit_code,
                exports,
                error,
            } => BinaryFrame::ExecutionResult {
                session_id,
                exit_code,
                exports,
                error,
            },
            RuntimeEvent::Log {
                session_id,
                channel,
                message,
            } => BinaryFrame::Log {
                session_id,
                channel,
                message,
            },
            RuntimeEvent::StreamCallback {
                session_id,
                callback_type,
                payload,
            } => BinaryFrame::StreamCallback {
                session_id,
                callback_type,
                payload,
            },
        }
    }
}

fn non_zero_option(value: u32) -> Option<u32> {
    if value == 0 {
        None
    } else {
        Some(value)
    }
}

pub fn validate_bridge_response_status(status: u8) -> io::Result<()> {
    if status <= 2 {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("unknown BridgeResponse status: {status}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_execute_mode() {
        let err = RuntimeCommand::try_from(BinaryFrame::Execute {
            session_id: "s".into(),
            mode: 4,
            file_path: "/app/main.mjs".into(),
            bridge_code: String::new(),
            post_restore_script: String::new(),
            userland_code: String::new(),
            high_resolution_time: false,
            user_code: String::new(),
        })
        .expect_err("unknown execute mode should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("unknown Execute mode"));
    }

    #[test]
    fn rejects_unknown_bridge_response_status() {
        let err = RuntimeCommand::try_from(BinaryFrame::BridgeResponse {
            session_id: "s".into(),
            call_id: 1,
            status: 3,
            payload: Vec::new(),
        })
        .expect_err("unknown bridge response status should be rejected");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("unknown BridgeResponse status"));
    }

    #[test]
    fn accepts_known_bridge_response_statuses() {
        for status in 0..=2 {
            let command = RuntimeCommand::try_from(BinaryFrame::BridgeResponse {
                session_id: "s".into(),
                call_id: 1,
                status,
                payload: Vec::new(),
            })
            .expect("known bridge response status should be accepted");

            assert!(matches!(
                command,
                RuntimeCommand::SendToSession {
                    message: SessionMessage::BridgeResponse(BridgeResponse { status: s, .. }),
                    ..
                } if s == status
            ));
        }
    }
}
