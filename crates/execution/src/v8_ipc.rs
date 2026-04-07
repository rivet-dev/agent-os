//! Binary IPC framing for communication with the agent-os-v8 runtime process.
//!
//! Wire format per frame:
//!   [4B total_len (u32 BE, excludes self)]
//!   [1B msg_type]
//!   [1B sid_len (N)]
//!   [N bytes session_id (UTF-8)]
//!   [... type-specific fixed fields ...]
//!   [M bytes payload (rest of frame)]

use std::io;

/// Maximum frame payload: 64 MB.
const MAX_FRAME_SIZE: u32 = 64 * 1024 * 1024;

// Host → V8 message type codes
const MSG_AUTHENTICATE: u8 = 0x01;
const MSG_CREATE_SESSION: u8 = 0x02;
const MSG_DESTROY_SESSION: u8 = 0x03;
const MSG_INJECT_GLOBALS: u8 = 0x04;
const MSG_EXECUTE: u8 = 0x05;
const MSG_BRIDGE_RESPONSE: u8 = 0x06;
const MSG_STREAM_EVENT: u8 = 0x07;
const MSG_TERMINATE_EXECUTION: u8 = 0x08;

// V8 → Host message type codes
const MSG_BRIDGE_CALL: u8 = 0x81;
const MSG_EXECUTION_RESULT: u8 = 0x82;
const MSG_LOG: u8 = 0x83;
const MSG_STREAM_CALLBACK: u8 = 0x84;

// ExecutionResult flags
const FLAG_HAS_EXPORTS: u8 = 0x01;
const FLAG_HAS_ERROR: u8 = 0x02;

/// A decoded binary frame.
#[derive(Debug, Clone, PartialEq)]
pub enum BinaryFrame {
    // Host → V8
    Authenticate {
        token: String,
    },
    CreateSession {
        session_id: String,
        heap_limit_mb: u32,
        cpu_time_limit_ms: u32,
    },
    DestroySession {
        session_id: String,
    },
    InjectGlobals {
        session_id: String,
        payload: Vec<u8>,
    },
    Execute {
        session_id: String,
        mode: u8, // 0 = exec (CJS), 1 = run (ESM)
        file_path: String,
        bridge_code: String,
        post_restore_script: String,
        user_code: String,
    },
    BridgeResponse {
        session_id: String,
        call_id: u64,
        status: u8,       // 0 = success, 1 = error, 2 = raw binary
        payload: Vec<u8>,
    },
    StreamEvent {
        session_id: String,
        event_type: String,
        payload: Vec<u8>,
    },
    TerminateExecution {
        session_id: String,
    },

    // V8 → Host
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
        channel: u8, // 0 = stdout, 1 = stderr
        message: String,
    },
    StreamCallback {
        session_id: String,
        callback_type: String,
        payload: Vec<u8>,
    },
}

/// Structured error from V8 execution.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionErrorBin {
    pub error_type: String,
    pub message: String,
    pub stack: String,
    pub code: String,
}

/// Encode a frame into a byte buffer (length prefix + body).
pub fn encode_frame(frame: &BinaryFrame) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    // Reserve 4 bytes for length prefix
    buf.extend_from_slice(&[0, 0, 0, 0]);
    encode_body(&mut buf, frame)?;
    let total_len = buf.len() - 4;
    if total_len > MAX_FRAME_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {total_len} exceeds maximum {MAX_FRAME_SIZE}"),
        ));
    }
    buf[..4].copy_from_slice(&(total_len as u32).to_be_bytes());
    Ok(buf)
}

/// Decode a frame from raw bytes (after the 4-byte length prefix has been read).
pub fn decode_frame(buf: &[u8]) -> io::Result<BinaryFrame> {
    if buf.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty frame"));
    }

    let msg_type = buf[0];
    let mut pos = 1;

    let sid_len = read_u8(buf, &mut pos)? as usize;
    let session_id = read_utf8(buf, &mut pos, sid_len)?;

    match msg_type {
        MSG_AUTHENTICATE => {
            let remaining = buf.len() - pos;
            let token = read_utf8(buf, &mut pos, remaining)?;
            Ok(BinaryFrame::Authenticate { token })
        }
        MSG_CREATE_SESSION => {
            let heap_limit_mb = read_u32(buf, &mut pos)?;
            let cpu_time_limit_ms = read_u32(buf, &mut pos)?;
            Ok(BinaryFrame::CreateSession {
                session_id,
                heap_limit_mb,
                cpu_time_limit_ms,
            })
        }
        MSG_DESTROY_SESSION => Ok(BinaryFrame::DestroySession { session_id }),
        MSG_INJECT_GLOBALS => {
            let payload = buf[pos..].to_vec();
            Ok(BinaryFrame::InjectGlobals {
                session_id,
                payload,
            })
        }
        MSG_EXECUTE => {
            let mode = read_u8(buf, &mut pos)?;
            let fp_len = read_u16(buf, &mut pos)? as usize;
            let file_path = read_utf8(buf, &mut pos, fp_len)?;
            let bc_len = read_u32(buf, &mut pos)? as usize;
            let bridge_code = read_utf8(buf, &mut pos, bc_len)?;
            let prs_len = read_u32(buf, &mut pos)? as usize;
            let post_restore_script = read_utf8(buf, &mut pos, prs_len)?;
            let remaining = buf.len() - pos;
            let user_code = read_utf8(buf, &mut pos, remaining)?;
            Ok(BinaryFrame::Execute {
                session_id,
                mode,
                file_path,
                bridge_code,
                post_restore_script,
                user_code,
            })
        }
        MSG_BRIDGE_RESPONSE => {
            let call_id = read_u64(buf, &mut pos)?;
            let status = read_u8(buf, &mut pos)?;
            let payload = buf[pos..].to_vec();
            Ok(BinaryFrame::BridgeResponse {
                session_id,
                call_id,
                status,
                payload,
            })
        }
        MSG_STREAM_EVENT => {
            let et_len = read_u16(buf, &mut pos)? as usize;
            let event_type = read_utf8(buf, &mut pos, et_len)?;
            let payload = buf[pos..].to_vec();
            Ok(BinaryFrame::StreamEvent {
                session_id,
                event_type,
                payload,
            })
        }
        MSG_TERMINATE_EXECUTION => Ok(BinaryFrame::TerminateExecution { session_id }),
        MSG_BRIDGE_CALL => {
            let call_id = read_u64(buf, &mut pos)?;
            let m_len = read_u16(buf, &mut pos)? as usize;
            let method = read_utf8(buf, &mut pos, m_len)?;
            let payload = buf[pos..].to_vec();
            Ok(BinaryFrame::BridgeCall {
                session_id,
                call_id,
                method,
                payload,
            })
        }
        MSG_EXECUTION_RESULT => {
            let exit_code = read_i32(buf, &mut pos)?;
            let flags = read_u8(buf, &mut pos)?;
            let exports = if flags & FLAG_HAS_EXPORTS != 0 {
                let exp_len = read_u32(buf, &mut pos)? as usize;
                let data = read_bytes(buf, &mut pos, exp_len)?;
                Some(data)
            } else {
                None
            };
            let error = if flags & FLAG_HAS_ERROR != 0 {
                let error_type = read_len_prefixed_u16(buf, &mut pos)?;
                let message = read_len_prefixed_u16(buf, &mut pos)?;
                let stack = read_len_prefixed_u16(buf, &mut pos)?;
                let code = read_len_prefixed_u16(buf, &mut pos)?;
                Some(ExecutionErrorBin {
                    error_type,
                    message,
                    stack,
                    code,
                })
            } else {
                None
            };
            Ok(BinaryFrame::ExecutionResult {
                session_id,
                exit_code,
                exports,
                error,
            })
        }
        MSG_LOG => {
            let channel = read_u8(buf, &mut pos)?;
            let remaining = buf.len() - pos;
            let message = read_utf8(buf, &mut pos, remaining)?;
            Ok(BinaryFrame::Log {
                session_id,
                channel,
                message,
            })
        }
        MSG_STREAM_CALLBACK => {
            let ct_len = read_u16(buf, &mut pos)? as usize;
            let callback_type = read_utf8(buf, &mut pos, ct_len)?;
            let payload = buf[pos..].to_vec();
            Ok(BinaryFrame::StreamCallback {
                session_id,
                callback_type,
                payload,
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown message type: 0x{msg_type:02x}"),
        )),
    }
}

// -- Encode body --

fn encode_body(buf: &mut Vec<u8>, frame: &BinaryFrame) -> io::Result<()> {
    match frame {
        BinaryFrame::Authenticate { token } => {
            buf.push(MSG_AUTHENTICATE);
            buf.push(0); // no session_id
            buf.extend_from_slice(token.as_bytes());
        }
        BinaryFrame::CreateSession {
            session_id,
            heap_limit_mb,
            cpu_time_limit_ms,
        } => {
            buf.push(MSG_CREATE_SESSION);
            write_session_id(buf, session_id)?;
            buf.extend_from_slice(&heap_limit_mb.to_be_bytes());
            buf.extend_from_slice(&cpu_time_limit_ms.to_be_bytes());
        }
        BinaryFrame::DestroySession { session_id } => {
            buf.push(MSG_DESTROY_SESSION);
            write_session_id(buf, session_id)?;
        }
        BinaryFrame::InjectGlobals {
            session_id,
            payload,
        } => {
            buf.push(MSG_INJECT_GLOBALS);
            write_session_id(buf, session_id)?;
            buf.extend_from_slice(payload);
        }
        BinaryFrame::Execute {
            session_id,
            mode,
            file_path,
            bridge_code,
            post_restore_script,
            user_code,
        } => {
            buf.push(MSG_EXECUTE);
            write_session_id(buf, session_id)?;
            buf.push(*mode);
            write_len_prefixed_u16(buf, file_path)?;
            let bc_bytes = bridge_code.as_bytes();
            buf.extend_from_slice(&(bc_bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(bc_bytes);
            let prs_bytes = post_restore_script.as_bytes();
            buf.extend_from_slice(&(prs_bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(prs_bytes);
            buf.extend_from_slice(user_code.as_bytes());
        }
        BinaryFrame::BridgeResponse {
            session_id,
            call_id,
            status,
            payload,
        } => {
            buf.push(MSG_BRIDGE_RESPONSE);
            write_session_id(buf, session_id)?;
            buf.extend_from_slice(&call_id.to_be_bytes());
            buf.push(*status);
            buf.extend_from_slice(payload);
        }
        BinaryFrame::StreamEvent {
            session_id,
            event_type,
            payload,
        } => {
            buf.push(MSG_STREAM_EVENT);
            write_session_id(buf, session_id)?;
            write_len_prefixed_u16(buf, event_type)?;
            buf.extend_from_slice(payload);
        }
        BinaryFrame::TerminateExecution { session_id } => {
            buf.push(MSG_TERMINATE_EXECUTION);
            write_session_id(buf, session_id)?;
        }
        // V8→Host frames: include encoding for completeness/testing
        BinaryFrame::BridgeCall {
            session_id,
            call_id,
            method,
            payload,
        } => {
            buf.push(MSG_BRIDGE_CALL);
            write_session_id(buf, session_id)?;
            buf.extend_from_slice(&call_id.to_be_bytes());
            write_len_prefixed_u16(buf, method)?;
            buf.extend_from_slice(payload);
        }
        BinaryFrame::ExecutionResult {
            session_id,
            exit_code,
            exports,
            error,
        } => {
            buf.push(MSG_EXECUTION_RESULT);
            write_session_id(buf, session_id)?;
            buf.extend_from_slice(&exit_code.to_be_bytes());
            let mut flags: u8 = 0;
            if exports.is_some() {
                flags |= FLAG_HAS_EXPORTS;
            }
            if error.is_some() {
                flags |= FLAG_HAS_ERROR;
            }
            buf.push(flags);
            if let Some(exp) = exports {
                buf.extend_from_slice(&(exp.len() as u32).to_be_bytes());
                buf.extend_from_slice(exp);
            }
            if let Some(err) = error {
                write_len_prefixed_u16(buf, &err.error_type)?;
                write_len_prefixed_u16(buf, &err.message)?;
                write_len_prefixed_u16(buf, &err.stack)?;
                write_len_prefixed_u16(buf, &err.code)?;
            }
        }
        BinaryFrame::Log {
            session_id,
            channel,
            message,
        } => {
            buf.push(MSG_LOG);
            write_session_id(buf, session_id)?;
            buf.push(*channel);
            buf.extend_from_slice(message.as_bytes());
        }
        BinaryFrame::StreamCallback {
            session_id,
            callback_type,
            payload,
        } => {
            buf.push(MSG_STREAM_CALLBACK);
            write_session_id(buf, session_id)?;
            write_len_prefixed_u16(buf, callback_type)?;
            buf.extend_from_slice(payload);
        }
    }
    Ok(())
}

// -- Primitive helpers --

fn write_session_id(buf: &mut Vec<u8>, sid: &str) -> io::Result<()> {
    let bytes = sid.as_bytes();
    if bytes.len() > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("session ID byte length {} exceeds u8 max 255", bytes.len()),
        ));
    }
    buf.push(bytes.len() as u8);
    buf.extend_from_slice(bytes);
    Ok(())
}

fn write_len_prefixed_u16(buf: &mut Vec<u8>, s: &str) -> io::Result<()> {
    let bytes = s.as_bytes();
    if bytes.len() > 0xFFFF {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("string byte length {} exceeds u16 max 65535", bytes.len()),
        ));
    }
    buf.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    buf.extend_from_slice(bytes);
    Ok(())
}

fn read_u8(buf: &[u8], pos: &mut usize) -> io::Result<u8> {
    if *pos >= buf.len() {
        return Err(eof());
    }
    let v = buf[*pos];
    *pos += 1;
    Ok(v)
}

fn read_u16(buf: &[u8], pos: &mut usize) -> io::Result<u16> {
    if *pos + 2 > buf.len() {
        return Err(eof());
    }
    let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
    *pos += 2;
    Ok(v)
}

fn read_u32(buf: &[u8], pos: &mut usize) -> io::Result<u32> {
    if *pos + 4 > buf.len() {
        return Err(eof());
    }
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_i32(buf: &[u8], pos: &mut usize) -> io::Result<i32> {
    if *pos + 4 > buf.len() {
        return Err(eof());
    }
    let v = i32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_u64(buf: &[u8], pos: &mut usize) -> io::Result<u64> {
    if *pos + 8 > buf.len() {
        return Err(eof());
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[*pos..*pos + 8]);
    *pos += 8;
    Ok(u64::from_be_bytes(bytes))
}

fn read_utf8(buf: &[u8], pos: &mut usize, len: usize) -> io::Result<String> {
    if *pos + len > buf.len() {
        return Err(eof());
    }
    let s = std::str::from_utf8(&buf[*pos..*pos + len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    *pos += len;
    Ok(s.to_owned())
}

fn read_bytes(buf: &[u8], pos: &mut usize, len: usize) -> io::Result<Vec<u8>> {
    if *pos + len > buf.len() {
        return Err(eof());
    }
    let data = buf[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(data)
}

fn read_len_prefixed_u16(buf: &[u8], pos: &mut usize) -> io::Result<String> {
    let len = read_u16(buf, pos)? as usize;
    read_utf8(buf, pos, len)
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "unexpected end of frame")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_authenticate() {
        let frame = BinaryFrame::Authenticate {
            token: "secret123".into(),
        };
        let bytes = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&bytes[4..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn roundtrip_create_session() {
        let frame = BinaryFrame::CreateSession {
            session_id: "sess-1".into(),
            heap_limit_mb: 256,
            cpu_time_limit_ms: 30000,
        };
        let bytes = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&bytes[4..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn roundtrip_bridge_call() {
        let frame = BinaryFrame::BridgeCall {
            session_id: "sess-1".into(),
            call_id: 42,
            method: "_fsReadFile".into(),
            payload: vec![1, 2, 3],
        };
        let bytes = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&bytes[4..]).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn roundtrip_execution_result_with_error() {
        let frame = BinaryFrame::ExecutionResult {
            session_id: "sess-1".into(),
            exit_code: 1,
            exports: None,
            error: Some(ExecutionErrorBin {
                error_type: "Error".into(),
                message: "something failed".into(),
                stack: "at foo:1:1".into(),
                code: "ERR_TEST".into(),
            }),
        };
        let bytes = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&bytes[4..]).unwrap();
        assert_eq!(frame, decoded);
    }
}
