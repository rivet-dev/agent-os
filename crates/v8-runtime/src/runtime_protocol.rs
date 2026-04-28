use crate::ipc_binary::{BinaryFrame, ExecutionErrorBin};
use std::io;

#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeCommand {
    CreateSession {
        session_id: String,
        heap_limit_mb: Option<u32>,
        cpu_time_limit_ms: Option<u32>,
    },
    DestroySession {
        session_id: String,
    },
    WarmSnapshot {
        bridge_code: String,
    },
    SendToSession {
        session_id: String,
        message: SessionMessage,
    },
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
        user_code: String,
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
            } => Ok(RuntimeCommand::CreateSession {
                session_id,
                heap_limit_mb: non_zero_option(heap_limit_mb),
                cpu_time_limit_ms: non_zero_option(cpu_time_limit_ms),
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
                user_code,
            } => Ok(RuntimeCommand::SendToSession {
                session_id,
                message: SessionMessage::Execute {
                    mode,
                    file_path,
                    bridge_code,
                    post_restore_script,
                    user_code,
                },
            }),
            BinaryFrame::BridgeResponse {
                session_id,
                call_id,
                status,
                payload,
            } => Ok(RuntimeCommand::SendToSession {
                session_id,
                message: SessionMessage::BridgeResponse(BridgeResponse {
                    call_id,
                    status,
                    payload,
                }),
            }),
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
            BinaryFrame::WarmSnapshot { bridge_code } => {
                Ok(RuntimeCommand::WarmSnapshot { bridge_code })
            }
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
