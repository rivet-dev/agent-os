// Binary header IPC framing — custom wire format for all message types.
//
// Wire format per frame:
//   [4B total_len (u32 BE, excludes self)]
//   [1B msg_type]
//   [1B sid_len (N)]
//   [N bytes session_id (UTF-8)]
//   [... type-specific fixed fields ...]
//   [M bytes payload (rest of frame)]
//
// Existing ipc.rs (MessagePack framing) is left unchanged.

use std::io::{self, Read, Write};

/// Maximum frame payload: 64 MB (same limit as MessagePack framing).
const MAX_FRAME_SIZE: u32 = 64 * 1024 * 1024;

// Host → Rust message type codes
const MSG_AUTHENTICATE: u8 = 0x01;
const MSG_CREATE_SESSION: u8 = 0x02;
const MSG_DESTROY_SESSION: u8 = 0x03;
const MSG_INJECT_GLOBALS: u8 = 0x04;
const MSG_EXECUTE: u8 = 0x05;
const MSG_BRIDGE_RESPONSE: u8 = 0x06;
const MSG_STREAM_EVENT: u8 = 0x07;
const MSG_TERMINATE_EXECUTION: u8 = 0x08;
const MSG_WARM_SNAPSHOT: u8 = 0x09;

// Rust → Host message type codes
const MSG_BRIDGE_CALL: u8 = 0x81;
const MSG_EXECUTION_RESULT: u8 = 0x82;
const MSG_LOG: u8 = 0x83;
const MSG_STREAM_CALLBACK: u8 = 0x84;

// ExecutionResult flags
const FLAG_HAS_EXPORTS: u8 = 0x01;
const FLAG_HAS_ERROR: u8 = 0x02;

/// A decoded binary frame — all fields are borrowed or owned depending on use.
#[derive(Debug, Clone, PartialEq)]
pub enum BinaryFrame {
    // Host → Rust
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
        payload: Vec<u8>, // V8-serialized { processConfig, osConfig }
    },
    Execute {
        session_id: String,
        mode: u8, // 0 = exec, 1 = run
        file_path: String,
        bridge_code: String,
        post_restore_script: String,
        user_code: String,
    },
    BridgeResponse {
        session_id: String,
        call_id: u64,
        status: u8,       // 0 = success, 1 = error
        payload: Vec<u8>, // V8-serialized result OR UTF-8 error message
    },
    StreamEvent {
        session_id: String,
        event_type: String,
        payload: Vec<u8>, // V8-serialized payload
    },
    TerminateExecution {
        session_id: String,
    },
    WarmSnapshot {
        bridge_code: String,
    },

    // Rust → Host
    BridgeCall {
        session_id: String,
        call_id: u64,
        method: String,
        payload: Vec<u8>, // V8-serialized args
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
        payload: Vec<u8>, // V8-serialized payload
    },
}

/// Structured error in binary format.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionErrorBin {
    pub error_type: String,
    pub message: String,
    pub stack: String,
    pub code: String, // empty string = no code
}

/// Encode a binary frame into a provided buffer (length prefix + body).
/// The buffer is cleared first; capacity is preserved across calls.
/// Used by per-session buffering to avoid per-call allocation.
pub fn encode_frame_into(buf: &mut Vec<u8>, frame: &BinaryFrame) -> io::Result<()> {
    buf.clear();
    // Reserve 4 bytes for the length prefix (filled after body)
    buf.extend_from_slice(&[0, 0, 0, 0]);
    encode_body(buf, frame)?;

    let total_len = buf.len() - 4;
    if total_len > MAX_FRAME_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {total_len} exceeds maximum {MAX_FRAME_SIZE}"),
        ));
    }
    buf[..4].copy_from_slice(&(total_len as u32).to_be_bytes());
    Ok(())
}

/// Serialize a binary frame to a complete byte vector (length prefix + body).
/// Used by per-session buffering to build the frame without holding any shared lock.
pub fn frame_to_bytes(frame: &BinaryFrame) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    encode_frame_into(&mut buf, frame)?;
    Ok(buf)
}

/// Write a binary frame to a writer.
pub fn write_frame<W: Write>(writer: &mut W, frame: &BinaryFrame) -> io::Result<()> {
    let bytes = frame_to_bytes(frame)?;
    writer.write_all(&bytes)?;
    Ok(())
}

/// Read a binary frame from a reader.
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<BinaryFrame> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let total_len = u32::from_be_bytes(len_buf);

    if total_len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {total_len} exceeds maximum {MAX_FRAME_SIZE}"),
        ));
    }

    let mut buf = vec![0u8; total_len as usize];
    reader.read_exact(&mut buf)?;
    decode_body(&buf)
}

/// Extract session_id from raw frame bytes without full deserialization.
/// `raw` starts at the first byte after the 4-byte length prefix (i.e. the msg_type byte).
/// Returns None for Authenticate (which has no session_id).
#[allow(dead_code)]
pub fn extract_session_id(raw: &[u8]) -> io::Result<Option<&str>> {
    if raw.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too short",
        ));
    }
    let msg_type = raw[0];
    if msg_type == MSG_AUTHENTICATE || msg_type == MSG_WARM_SNAPSHOT {
        return Ok(None);
    }
    let sid_len = raw[1] as usize;
    if raw.len() < 2 + sid_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too short for session_id",
        ));
    }
    let sid = std::str::from_utf8(&raw[2..2 + sid_len])
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(Some(sid))
}

// -- Internal encode/decode --

fn encode_body(buf: &mut Vec<u8>, frame: &BinaryFrame) -> io::Result<()> {
    match frame {
        BinaryFrame::Authenticate { token } => {
            buf.push(MSG_AUTHENTICATE);
            // Authenticate has no session_id — sid_len = 0
            buf.push(0);
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
            // file_path length (u16 BE)
            write_len_prefixed_u16(buf, file_path)?;
            // bridge_code length (u32 BE)
            let bc_bytes = bridge_code.as_bytes();
            buf.extend_from_slice(&(bc_bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(bc_bytes);
            // post_restore_script length (u32 BE)
            let prs_bytes = post_restore_script.as_bytes();
            buf.extend_from_slice(&(prs_bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(prs_bytes);
            // user_code (rest of frame)
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
        BinaryFrame::WarmSnapshot { bridge_code } => {
            buf.push(MSG_WARM_SNAPSHOT);
            buf.push(0); // no session_id
            let bc_bytes = bridge_code.as_bytes();
            buf.extend_from_slice(&(bc_bytes.len() as u32).to_be_bytes());
            buf.extend_from_slice(bc_bytes);
        }
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

fn decode_body(buf: &[u8]) -> io::Result<BinaryFrame> {
    if buf.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "empty frame"));
    }

    let msg_type = buf[0];
    let mut pos = 1;

    // Read session_id (all types except Authenticate have it, but we read the field uniformly)
    let sid_len = read_u8(buf, &mut pos)? as usize;
    let session_id = read_utf8(buf, &mut pos, sid_len)?;

    match msg_type {
        MSG_AUTHENTICATE => {
            // Token is rest of frame after sid (sid is empty for Authenticate)
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
        MSG_WARM_SNAPSHOT => {
            let bc_len = read_u32(buf, &mut pos)? as usize;
            let bridge_code = read_utf8(buf, &mut pos, bc_len)?;
            Ok(BinaryFrame::WarmSnapshot { bridge_code })
        }
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

// -- Primitive read/write helpers --

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
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = buf[*pos];
    *pos += 1;
    Ok(v)
}

fn read_u16(buf: &[u8], pos: &mut usize) -> io::Result<u16> {
    if *pos + 2 > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = u16::from_be_bytes([buf[*pos], buf[*pos + 1]]);
    *pos += 2;
    Ok(v)
}

fn read_u32(buf: &[u8], pos: &mut usize) -> io::Result<u32> {
    if *pos + 4 > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = u32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_u64(buf: &[u8], pos: &mut usize) -> io::Result<u64> {
    if *pos + 8 > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = u64::from_be_bytes([
        buf[*pos],
        buf[*pos + 1],
        buf[*pos + 2],
        buf[*pos + 3],
        buf[*pos + 4],
        buf[*pos + 5],
        buf[*pos + 6],
        buf[*pos + 7],
    ]);
    *pos += 8;
    Ok(v)
}

fn read_i32(buf: &[u8], pos: &mut usize) -> io::Result<i32> {
    if *pos + 4 > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = i32::from_be_bytes([buf[*pos], buf[*pos + 1], buf[*pos + 2], buf[*pos + 3]]);
    *pos += 4;
    Ok(v)
}

fn read_bytes(buf: &[u8], pos: &mut usize, len: usize) -> io::Result<Vec<u8>> {
    if *pos + len > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected end of frame",
        ));
    }
    let v = buf[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(v)
}

fn read_utf8(buf: &[u8], pos: &mut usize, len: usize) -> io::Result<String> {
    let bytes = read_bytes(buf, pos, len)?;
    String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn read_len_prefixed_u16(buf: &[u8], pos: &mut usize) -> io::Result<String> {
    let len = read_u16(buf, pos)? as usize;
    read_utf8(buf, pos, len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: &BinaryFrame) {
        let mut buf = Vec::new();
        write_frame(&mut buf, frame).expect("write_frame");
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = read_frame(&mut cursor).expect("read_frame");
        assert_eq!(&decoded, frame);
    }

    // -- Host → Rust message types --

    #[test]
    fn roundtrip_authenticate() {
        roundtrip(&BinaryFrame::Authenticate {
            token: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4".into(),
        });
    }

    #[test]
    fn roundtrip_create_session() {
        roundtrip(&BinaryFrame::CreateSession {
            session_id: "sess-abc-123".into(),
            heap_limit_mb: 128,
            cpu_time_limit_ms: 5000,
        });
    }

    #[test]
    fn roundtrip_create_session_no_limits() {
        roundtrip(&BinaryFrame::CreateSession {
            session_id: "sess-1".into(),
            heap_limit_mb: 0,
            cpu_time_limit_ms: 0,
        });
    }

    #[test]
    fn roundtrip_destroy_session() {
        roundtrip(&BinaryFrame::DestroySession {
            session_id: "sess-7".into(),
        });
    }

    #[test]
    fn roundtrip_inject_globals() {
        roundtrip(&BinaryFrame::InjectGlobals {
            session_id: "sess-3".into(),
            payload: vec![0x01, 0x02, 0x03, 0x04, 0x05],
        });
    }

    #[test]
    fn roundtrip_execute_exec_mode() {
        roundtrip(&BinaryFrame::Execute {
            session_id: "sess-1".into(),
            mode: 0,
            file_path: "".into(),
            bridge_code: "(function(){ /* bridge */ })()".into(),
            post_restore_script: "".into(),
            user_code: "console.log('hello')".into(),
        });
    }

    #[test]
    fn roundtrip_execute_run_mode() {
        roundtrip(&BinaryFrame::Execute {
            session_id: "sess-2".into(),
            mode: 1,
            file_path: "/app/index.mjs".into(),
            bridge_code: "(function(){ /* bridge */ })()".into(),
            post_restore_script: "__runtimeApplyConfig({})".into(),
            user_code: "export default 42".into(),
        });
    }

    #[test]
    fn roundtrip_bridge_response_success() {
        roundtrip(&BinaryFrame::BridgeResponse {
            session_id: "sess-4".into(),
            call_id: 100,
            status: 0,
            payload: vec![0x93, 0x01, 0x02, 0x03],
        });
    }

    #[test]
    fn roundtrip_bridge_response_error() {
        roundtrip(&BinaryFrame::BridgeResponse {
            session_id: "sess-5".into(),
            call_id: 101,
            status: 1,
            payload: b"ENOENT: no such file".to_vec(),
        });
    }

    #[test]
    fn roundtrip_stream_event() {
        roundtrip(&BinaryFrame::StreamEvent {
            session_id: "sess-5".into(),
            event_type: "child_stdout".into(),
            payload: vec![0x48, 0x65, 0x6c, 0x6c, 0x6f],
        });
    }

    #[test]
    fn roundtrip_terminate_execution() {
        roundtrip(&BinaryFrame::TerminateExecution {
            session_id: "sess-6".into(),
        });
    }

    // -- Rust → Host message types --

    #[test]
    fn roundtrip_bridge_call() {
        roundtrip(&BinaryFrame::BridgeCall {
            session_id: "sess-1".into(),
            call_id: 200,
            method: "_fsReadFile".into(),
            payload: vec![0x91, 0xa5, 0x2f, 0x74, 0x6d, 0x70],
        });
    }

    #[test]
    fn roundtrip_execution_result_success() {
        roundtrip(&BinaryFrame::ExecutionResult {
            session_id: "sess-1".into(),
            exit_code: 0,
            exports: Some(vec![0xc0]),
            error: None,
        });
    }

    #[test]
    fn roundtrip_execution_result_error() {
        roundtrip(&BinaryFrame::ExecutionResult {
            session_id: "sess-2".into(),
            exit_code: 1,
            exports: None,
            error: Some(ExecutionErrorBin {
                error_type: "TypeError".into(),
                message: "Cannot read properties of undefined".into(),
                stack: "TypeError: Cannot read properties of undefined\n    at main.js:1:5".into(),
                code: "".into(),
            }),
        });
    }

    #[test]
    fn roundtrip_execution_result_error_with_code() {
        roundtrip(&BinaryFrame::ExecutionResult {
            session_id: "sess-3".into(),
            exit_code: 1,
            exports: None,
            error: Some(ExecutionErrorBin {
                error_type: "Error".into(),
                message: "Cannot find module './missing'".into(),
                stack: "Error: Cannot find module './missing'\n    at resolve (node:internal)"
                    .into(),
                code: "ERR_MODULE_NOT_FOUND".into(),
            }),
        });
    }

    #[test]
    fn roundtrip_execution_result_exports_and_error() {
        roundtrip(&BinaryFrame::ExecutionResult {
            session_id: "sess-4".into(),
            exit_code: 1,
            exports: Some(vec![0x01, 0x02]),
            error: Some(ExecutionErrorBin {
                error_type: "Error".into(),
                message: "partial failure".into(),
                stack: "".into(),
                code: "".into(),
            }),
        });
    }

    #[test]
    fn roundtrip_execution_result_no_exports_no_error() {
        roundtrip(&BinaryFrame::ExecutionResult {
            session_id: "sess-5".into(),
            exit_code: 0,
            exports: None,
            error: None,
        });
    }

    #[test]
    fn roundtrip_log_stdout() {
        roundtrip(&BinaryFrame::Log {
            session_id: "sess-1".into(),
            channel: 0,
            message: "hello world\n".into(),
        });
    }

    #[test]
    fn roundtrip_log_stderr() {
        roundtrip(&BinaryFrame::Log {
            session_id: "sess-1".into(),
            channel: 1,
            message: "warning: deprecated API\n".into(),
        });
    }

    #[test]
    fn roundtrip_stream_callback() {
        roundtrip(&BinaryFrame::StreamCallback {
            session_id: "sess-1".into(),
            callback_type: "child_dispatch".into(),
            payload: vec![0x92, 0x01, 0xa3, 0x66, 0x6f, 0x6f],
        });
    }

    // -- WarmSnapshot --

    #[test]
    fn roundtrip_warm_snapshot() {
        roundtrip(&BinaryFrame::WarmSnapshot {
            bridge_code: "(function(){ /* bridge IIFE */ })()".into(),
        });
    }

    #[test]
    fn roundtrip_warm_snapshot_empty_bridge_code() {
        roundtrip(&BinaryFrame::WarmSnapshot {
            bridge_code: "".into(),
        });
    }

    #[test]
    fn roundtrip_warm_snapshot_large_bridge_code() {
        roundtrip(&BinaryFrame::WarmSnapshot {
            bridge_code: "x".repeat(100_000),
        });
    }

    #[test]
    fn extract_session_id_warm_snapshot_returns_none() {
        let frame = BinaryFrame::WarmSnapshot {
            bridge_code: "bridge()".into(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write");
        let raw = &buf[4..];
        let result = extract_session_id(raw).expect("extract");
        assert_eq!(result, None);
    }

    // -- Edge cases --

    #[test]
    fn roundtrip_empty_payloads() {
        roundtrip(&BinaryFrame::BridgeResponse {
            session_id: "s".into(),
            call_id: 0,
            status: 0,
            payload: vec![],
        });
        roundtrip(&BinaryFrame::StreamEvent {
            session_id: "s".into(),
            event_type: "".into(),
            payload: vec![],
        });
        roundtrip(&BinaryFrame::BridgeCall {
            session_id: "s".into(),
            call_id: 0,
            method: "".into(),
            payload: vec![],
        });
        roundtrip(&BinaryFrame::InjectGlobals {
            session_id: "s".into(),
            payload: vec![],
        });
    }

    #[test]
    fn roundtrip_empty_session_id() {
        roundtrip(&BinaryFrame::DestroySession {
            session_id: "".into(),
        });
    }

    #[test]
    fn roundtrip_large_binary_payload() {
        roundtrip(&BinaryFrame::BridgeResponse {
            session_id: "sess-big".into(),
            call_id: 42,
            status: 0,
            payload: vec![0xAA; 1024],
        });
    }

    // -- Framing validation --

    #[test]
    fn frame_length_prefix_is_big_endian() {
        let frame = BinaryFrame::DestroySession {
            session_id: "x".into(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write");
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len as usize, buf.len() - 4);
    }

    #[test]
    fn multiple_frames_in_stream() {
        let frames = vec![
            BinaryFrame::CreateSession {
                session_id: "a".into(),
                heap_limit_mb: 64,
                cpu_time_limit_ms: 1000,
            },
            BinaryFrame::Execute {
                session_id: "a".into(),
                mode: 0,
                file_path: "".into(),
                bridge_code: "bridge()".into(),
                post_restore_script: "".into(),
                user_code: "1+1".into(),
            },
            BinaryFrame::DestroySession {
                session_id: "a".into(),
            },
        ];
        let mut buf = Vec::new();
        for f in &frames {
            write_frame(&mut buf, f).expect("write");
        }
        let mut cursor = std::io::Cursor::new(&buf);
        for f in &frames {
            let decoded = read_frame(&mut cursor).expect("read");
            assert_eq!(&decoded, f);
        }
    }

    #[test]
    fn reject_oversized_frame() {
        let oversized_len: u32 = 64 * 1024 * 1024 + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&oversized_len.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]);
        let mut cursor = std::io::Cursor::new(&buf);
        let result = read_frame(&mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("exceeds maximum"));
    }

    #[test]
    fn reject_unknown_message_type() {
        // Craft a frame with unknown message type 0xFF
        let body = vec![0xFF, 0x00]; // msg_type=0xFF, sid_len=0
        let mut buf = Vec::new();
        buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
        buf.extend_from_slice(&body);
        let mut cursor = std::io::Cursor::new(&buf);
        let result = read_frame(&mut cursor);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unknown message type"));
    }

    #[test]
    fn empty_input_returns_eof() {
        let buf: Vec<u8> = Vec::new();
        let mut cursor = std::io::Cursor::new(&buf);
        let result = read_frame(&mut cursor);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    // -- Session ID routing --

    #[test]
    fn extract_session_id_from_raw_bytes() {
        // Build a BridgeCall frame and verify we can extract session_id from raw bytes
        let frame = BinaryFrame::BridgeCall {
            session_id: "my-session-42".into(),
            call_id: 7,
            method: "_fsReadFile".into(),
            payload: vec![0x01, 0x02],
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write");

        // Raw bytes start after the 4-byte length prefix
        let raw = &buf[4..];
        let sid = extract_session_id(raw)
            .expect("extract")
            .expect("should have sid");
        assert_eq!(sid, "my-session-42");
    }

    #[test]
    fn extract_session_id_from_various_types() {
        let test_cases: Vec<BinaryFrame> = vec![
            BinaryFrame::CreateSession {
                session_id: "sess-create".into(),
                heap_limit_mb: 0,
                cpu_time_limit_ms: 0,
            },
            BinaryFrame::DestroySession {
                session_id: "sess-destroy".into(),
            },
            BinaryFrame::Execute {
                session_id: "sess-exec".into(),
                mode: 0,
                file_path: "".into(),
                bridge_code: "".into(),
                post_restore_script: "".into(),
                user_code: "".into(),
            },
            BinaryFrame::BridgeResponse {
                session_id: "sess-resp".into(),
                call_id: 1,
                status: 0,
                payload: vec![],
            },
            BinaryFrame::ExecutionResult {
                session_id: "sess-result".into(),
                exit_code: 0,
                exports: None,
                error: None,
            },
            BinaryFrame::Log {
                session_id: "sess-log".into(),
                channel: 0,
                message: "hi".into(),
            },
        ];

        for frame in &test_cases {
            let mut buf = Vec::new();
            write_frame(&mut buf, frame).expect("write");
            let raw = &buf[4..];
            let sid = extract_session_id(raw)
                .expect("extract")
                .expect("should have sid");
            // Verify it matches the expected session_id
            let expected = match frame {
                BinaryFrame::CreateSession { session_id, .. }
                | BinaryFrame::DestroySession { session_id }
                | BinaryFrame::Execute { session_id, .. }
                | BinaryFrame::BridgeResponse { session_id, .. }
                | BinaryFrame::ExecutionResult { session_id, .. }
                | BinaryFrame::Log { session_id, .. } => session_id.as_str(),
                _ => unreachable!(),
            };
            assert_eq!(sid, expected, "session_id mismatch for frame: {:?}", frame);
        }
    }

    #[test]
    fn extract_session_id_authenticate_returns_none() {
        let frame = BinaryFrame::Authenticate {
            token: "secret-token".into(),
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write");
        let raw = &buf[4..];
        let result = extract_session_id(raw).expect("extract");
        assert_eq!(result, None);
    }

    #[test]
    fn extract_session_id_too_short() {
        let result = extract_session_id(&[0x02]); // msg_type only, no sid_len
        assert!(result.is_err());
    }

    // -- Wire format byte-level verification --

    #[test]
    fn wire_format_message_type_bytes() {
        let cases: Vec<(BinaryFrame, u8)> = vec![
            (BinaryFrame::Authenticate { token: "t".into() }, 0x01),
            (
                BinaryFrame::CreateSession {
                    session_id: "s".into(),
                    heap_limit_mb: 0,
                    cpu_time_limit_ms: 0,
                },
                0x02,
            ),
            (
                BinaryFrame::DestroySession {
                    session_id: "s".into(),
                },
                0x03,
            ),
            (
                BinaryFrame::InjectGlobals {
                    session_id: "s".into(),
                    payload: vec![],
                },
                0x04,
            ),
            (
                BinaryFrame::Execute {
                    session_id: "s".into(),
                    mode: 0,
                    file_path: "".into(),
                    bridge_code: "".into(),
                    post_restore_script: "".into(),
                    user_code: "".into(),
                },
                0x05,
            ),
            (
                BinaryFrame::BridgeResponse {
                    session_id: "s".into(),
                    call_id: 0,
                    status: 0,
                    payload: vec![],
                },
                0x06,
            ),
            (
                BinaryFrame::StreamEvent {
                    session_id: "s".into(),
                    event_type: "".into(),
                    payload: vec![],
                },
                0x07,
            ),
            (
                BinaryFrame::TerminateExecution {
                    session_id: "s".into(),
                },
                0x08,
            ),
            (
                BinaryFrame::WarmSnapshot {
                    bridge_code: "bridge()".into(),
                },
                0x09,
            ),
            (
                BinaryFrame::BridgeCall {
                    session_id: "s".into(),
                    call_id: 0,
                    method: "".into(),
                    payload: vec![],
                },
                0x81,
            ),
            (
                BinaryFrame::ExecutionResult {
                    session_id: "s".into(),
                    exit_code: 0,
                    exports: None,
                    error: None,
                },
                0x82,
            ),
            (
                BinaryFrame::Log {
                    session_id: "s".into(),
                    channel: 0,
                    message: "".into(),
                },
                0x83,
            ),
            (
                BinaryFrame::StreamCallback {
                    session_id: "s".into(),
                    callback_type: "".into(),
                    payload: vec![],
                },
                0x84,
            ),
        ];
        for (frame, expected_type) in &cases {
            let mut buf = Vec::new();
            write_frame(&mut buf, frame).expect("write");
            // Byte 4 (after 4-byte length prefix) is the message type
            assert_eq!(buf[4], *expected_type, "type mismatch for: {:?}", frame);
        }
    }

    // -- frame_to_bytes tests --

    #[test]
    fn frame_to_bytes_matches_write_frame() {
        let frame = BinaryFrame::BridgeCall {
            session_id: "sess-42".into(),
            call_id: 123,
            method: "_fsReadFile".into(),
            payload: vec![0x01, 0x02, 0x03],
        };
        let bytes = frame_to_bytes(&frame).expect("frame_to_bytes");
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).expect("write_frame");
        assert_eq!(bytes, buf);
    }

    #[test]
    fn frame_to_bytes_roundtrip() {
        let frame = BinaryFrame::ExecutionResult {
            session_id: "sess-1".into(),
            exit_code: 0,
            exports: Some(vec![0xAA, 0xBB]),
            error: None,
        };
        let bytes = frame_to_bytes(&frame).expect("frame_to_bytes");
        let mut cursor = std::io::Cursor::new(&bytes);
        let decoded = read_frame(&mut cursor).expect("read_frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn frame_to_bytes_atomic_no_interleaving() {
        // Verify frame_to_bytes produces a single contiguous byte vector
        // (no intermediate writes that could interleave)
        let frame = BinaryFrame::BridgeCall {
            session_id: "s".into(),
            call_id: 1,
            method: "_fn".into(),
            payload: vec![0xFF; 1024],
        };
        let bytes = frame_to_bytes(&frame).expect("frame_to_bytes");
        // Length prefix matches body
        let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        assert_eq!(len, bytes.len() - 4);
    }

    #[test]
    fn encode_frame_into_reuses_buffer_capacity() {
        let mut buf = Vec::new();
        let frame = BinaryFrame::BridgeCall {
            session_id: "s1".into(),
            call_id: 1,
            method: "_fn".into(),
            payload: vec![0xAA; 512],
        };

        // First encode grows the buffer
        encode_frame_into(&mut buf, &frame).expect("encode");
        let first_bytes = buf.clone();
        let cap_after_first = buf.capacity();
        assert!(cap_after_first >= buf.len());

        // Second encode reuses capacity (no new allocation if same size)
        let frame2 = BinaryFrame::BridgeCall {
            session_id: "s1".into(),
            call_id: 2,
            method: "_fn".into(),
            payload: vec![0xBB; 256],
        };
        encode_frame_into(&mut buf, &frame2).expect("encode");
        assert!(
            buf.capacity() >= cap_after_first,
            "capacity should not shrink"
        );

        // Verify round-trip correctness
        let decoded = read_frame(&mut std::io::Cursor::new(&first_bytes)).expect("decode");
        assert_eq!(decoded, frame);
        let decoded2 = read_frame(&mut std::io::Cursor::new(&buf)).expect("decode");
        assert_eq!(decoded2, frame2);
    }

    #[test]
    fn encode_frame_into_matches_frame_to_bytes() {
        let frame = BinaryFrame::ExecutionResult {
            session_id: "sess-1".into(),
            exit_code: 0,
            exports: Some(vec![0x01, 0x02]),
            error: None,
        };
        let expected = frame_to_bytes(&frame).expect("frame_to_bytes");
        let mut buf = Vec::new();
        encode_frame_into(&mut buf, &frame).expect("encode_frame_into");
        assert_eq!(buf, expected);
    }

    #[test]
    fn encode_frame_into_grows_to_high_water_mark() {
        let mut buf = Vec::new();

        // Small frame
        let small = BinaryFrame::Log {
            session_id: "s".into(),
            channel: 0,
            message: "hi".into(),
        };
        encode_frame_into(&mut buf, &small).expect("encode");
        let small_cap = buf.capacity();

        // Large frame grows buffer
        let large = BinaryFrame::BridgeCall {
            session_id: "s".into(),
            call_id: 1,
            method: "_fn".into(),
            payload: vec![0xFF; 4096],
        };
        encode_frame_into(&mut buf, &large).expect("encode");
        let large_cap = buf.capacity();
        assert!(large_cap > small_cap);

        // Small frame again — capacity stays at high-water mark
        encode_frame_into(&mut buf, &small).expect("encode");
        assert_eq!(
            buf.capacity(),
            large_cap,
            "capacity should stay at high-water mark"
        );
    }

    // -- Overflow guard tests --

    #[test]
    fn write_session_id_rejects_oversized() {
        // Session ID > 255 bytes must be rejected
        let long_sid = "x".repeat(256);
        let frame = BinaryFrame::DestroySession {
            session_id: long_sid,
        };
        let result = frame_to_bytes(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("session ID byte length"));
        assert!(err.to_string().contains("255"));
    }

    #[test]
    fn write_session_id_accepts_max() {
        // Session ID of exactly 255 bytes must succeed
        let max_sid = "a".repeat(255);
        let frame = BinaryFrame::DestroySession {
            session_id: max_sid.clone(),
        };
        let bytes = frame_to_bytes(&frame).expect("should accept 255-byte session ID");
        let decoded = read_frame(&mut std::io::Cursor::new(&bytes)).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn write_len_prefixed_u16_rejects_oversized() {
        // String > 65535 bytes in a u16-prefixed field must be rejected
        let long_method = "m".repeat(65536);
        let frame = BinaryFrame::BridgeCall {
            session_id: "s".into(),
            call_id: 1,
            method: long_method,
            payload: vec![],
        };
        let result = frame_to_bytes(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("string byte length"));
        assert!(err.to_string().contains("65535"));
    }

    #[test]
    fn write_len_prefixed_u16_accepts_max() {
        // String of exactly 65535 bytes in a u16-prefixed field must succeed
        let max_method = "m".repeat(65535);
        let frame = BinaryFrame::BridgeCall {
            session_id: "s".into(),
            call_id: 1,
            method: max_method.clone(),
            payload: vec![],
        };
        let bytes = frame_to_bytes(&frame).expect("should accept 65535-byte method");
        let decoded = read_frame(&mut std::io::Cursor::new(&bytes)).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn execute_file_path_rejects_oversized() {
        // file_path > 65535 bytes must be rejected (encoded as u16)
        let long_path = "/".repeat(65536);
        let frame = BinaryFrame::Execute {
            session_id: "s".into(),
            mode: 0,
            file_path: long_path,
            bridge_code: "".into(),
            post_restore_script: "".into(),
            user_code: "".into(),
        };
        let result = frame_to_bytes(&frame);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("65535"));
    }
}
