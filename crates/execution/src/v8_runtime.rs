//! V8 isolate runtime manager backed by the embedded V8 runtime.

use crate::v8_ipc::{self, BinaryFrame};
use agentos_runtime::RuntimeContext;
use agentos_v8_runtime::embedded_runtime::{spawn_embedded_runtime_ipc, EmbeddedRuntimeHandle};
use serde_json::Value;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

/// Manages an embedded V8 runtime and its IPC connection.
pub struct V8Runtime {
    runtime: EmbeddedRuntimeHandle,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl V8Runtime {
    /// Spawn the embedded V8 runtime and connect over IPC.
    pub fn spawn(runtime_context: &RuntimeContext) -> io::Result<Self> {
        let (stream, runtime) = spawn_embedded_runtime_ipc(None, runtime_context.clone())?;
        let writer = stream.try_clone()?;
        let reader = BufReader::new(stream);

        Ok(V8Runtime {
            runtime,
            reader,
            writer,
        })
    }

    /// Create a new V8 isolate session.
    pub fn create_session(
        &mut self,
        session_id: &str,
        heap_limit_mb: u32,
        cpu_time_limit_ms: u32,
        wall_clock_limit_ms: u32,
    ) -> io::Result<()> {
        self.send_frame(&BinaryFrame::CreateSession {
            session_id: session_id.to_owned(),
            heap_limit_mb,
            cpu_time_limit_ms,
            wall_clock_limit_ms,
        })
    }

    /// Inject per-session globals (processConfig, osConfig) as CBOR payload.
    pub fn inject_globals(&mut self, session_id: &str, payload: Vec<u8>) -> io::Result<()> {
        self.send_frame(&BinaryFrame::InjectGlobals {
            session_id: session_id.to_owned(),
            payload,
        })
    }

    /// Execute bridge code + user code in a session.
    pub fn execute(
        &mut self,
        session_id: &str,
        mode: u8,
        file_path: &str,
        bridge_code: &str,
        user_code: &str,
    ) -> io::Result<()> {
        self.send_frame(&BinaryFrame::Execute {
            session_id: session_id.to_owned(),
            mode,
            file_path: file_path.to_owned(),
            bridge_code: bridge_code.to_owned(),
            post_restore_script: String::new(),
            userland_code: String::new(),
            high_resolution_time: false,
            user_code: user_code.to_owned(),
        })
    }

    /// Send a bridge response back to the V8 isolate.
    pub fn send_bridge_response(
        &mut self,
        session_id: &str,
        call_id: u64,
        status: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        self.send_frame(&BinaryFrame::BridgeResponse {
            session_id: session_id.to_owned(),
            call_id,
            status,
            payload,
        })
    }

    /// Send a stream event to the V8 isolate (stdin data, timer, child process events).
    pub fn send_stream_event(
        &mut self,
        session_id: &str,
        event_type: &str,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        self.send_frame(&BinaryFrame::StreamEvent {
            session_id: session_id.to_owned(),
            event_type: event_type.to_owned(),
            payload,
        })
    }

    /// Terminate execution in a session.
    pub fn terminate_execution(&mut self, session_id: &str) -> io::Result<()> {
        self.send_frame(&BinaryFrame::TerminateExecution {
            session_id: session_id.to_owned(),
        })
    }

    /// Destroy a session.
    pub fn destroy_session(&mut self, session_id: &str) -> io::Result<()> {
        self.send_frame(&BinaryFrame::DestroySession {
            session_id: session_id.to_owned(),
        })
    }

    /// Read the next frame from the V8 runtime.
    pub fn read_frame(&mut self) -> io::Result<BinaryFrame> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf)?;
        let total_len = u32::from_be_bytes(len_buf);

        if total_len > 64 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("frame size {total_len} exceeds maximum"),
            ));
        }

        let mut buf = vec![0u8; total_len as usize];
        self.reader.read_exact(&mut buf)?;
        v8_ipc::decode_frame(&buf)
    }

    fn send_frame(&mut self, frame: &BinaryFrame) -> io::Result<()> {
        let bytes = v8_ipc::encode_frame(frame)?;
        self.writer.write_all(&bytes)?;
        self.writer.flush()
    }
}

impl Drop for V8Runtime {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

/// Thread-safe wrapper for V8Runtime that allows sending from multiple threads.
pub struct SharedV8Runtime {
    inner: Arc<Mutex<V8Runtime>>,
}

impl SharedV8Runtime {
    pub fn new(runtime: V8Runtime) -> Self {
        Self {
            inner: Arc::new(Mutex::new(runtime)),
        }
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, V8Runtime> {
        self.inner.lock().expect("V8 runtime lock poisoned")
    }
}

impl Clone for SharedV8Runtime {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// Bridge call method name mapping from V8 polyfill names to sidecar sync RPC names.
/// The V8 polyfills use underscore-prefixed camelCase names while the sidecar
/// uses dot-separated category.method names. The mapping lives in
/// `bridge-contract.json` so bridge installation and dispatch drift together.
pub fn map_bridge_method(method: &str) -> (&str, bool) {
    if let Some(target) = agentos_bridge::bridge_contract().dispatch.get(method) {
        (target.method.as_str(), target.translate_args)
    } else {
        (method, false)
    }
}

/// Deserialize a CBOR payload into a JSON array of arguments.
/// The V8 bridge serializes bridge call args as a CBOR array.
pub fn cbor_payload_to_json_args(payload: &[u8]) -> io::Result<Vec<Value>> {
    if payload.is_empty() {
        return Ok(vec![]);
    }
    let cbor_value: ciborium::value::Value = ciborium::de::from_reader(payload).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to deserialize CBOR bridge call payload: {e}"),
        )
    })?;
    match cbor_to_json(cbor_value) {
        Value::Array(arr) => Ok(arr),
        single => Ok(vec![single]),
    }
}

pub fn cbor_payload_raw_byte_arg(payload: &[u8], index: usize) -> io::Result<Option<Vec<u8>>> {
    if payload.is_empty() {
        return Ok(None);
    }
    let cbor_value: ciborium::value::Value = ciborium::de::from_reader(payload).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to deserialize CBOR bridge call payload: {e}"),
        )
    })?;
    let Some(value) = cbor_array_arg(&cbor_value, index) else {
        return Ok(None);
    };
    Ok(cbor_raw_bytes(value).map(ToOwned::to_owned))
}

/// Serialize a JSON value to CBOR bytes for bridge responses.
pub fn json_to_cbor_payload(value: &Value) -> io::Result<Vec<u8>> {
    let cbor_value = json_to_cbor(value);
    let mut buf = Vec::new();
    ciborium::ser::into_writer(&cbor_value, &mut buf).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize CBOR bridge response: {e}"),
        )
    })?;
    Ok(buf)
}

fn cbor_array_arg(value: &ciborium::value::Value, index: usize) -> Option<&ciborium::value::Value> {
    match value {
        ciborium::value::Value::Array(values) => values.get(index),
        value if index == 0 => Some(value),
        _ => None,
    }
}

fn cbor_raw_bytes(value: &ciborium::value::Value) -> Option<&[u8]> {
    match value {
        ciborium::value::Value::Bytes(bytes) => Some(bytes),
        ciborium::value::Value::Tag(_, inner) => cbor_raw_bytes(inner),
        _ => None,
    }
}

fn cbor_to_json(value: ciborium::value::Value) -> Value {
    use ciborium::value::Value as Cbor;
    match value {
        Cbor::Null => Value::Null,
        Cbor::Bool(b) => Value::Bool(b),
        Cbor::Integer(i) => {
            let n: i128 = i.into();
            if let Ok(n) = i64::try_from(n) {
                Value::Number(n.into())
            } else if let Ok(n) = u64::try_from(n) {
                Value::Number(n.into())
            } else {
                Value::Number(serde_json::Number::from_f64(n as f64).unwrap_or(0.into()))
            }
        }
        Cbor::Float(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Cbor::Text(s) => Value::String(s),
        Cbor::Bytes(b) => {
            use serde_json::json;
            // Encode binary data as base64 with a type marker
            json!({ "__type": "Buffer", "data": base64_encode(&b) })
        }
        Cbor::Array(arr) => Value::Array(arr.into_iter().map(cbor_to_json).collect()),
        Cbor::Map(map) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in map {
                let key = match k {
                    Cbor::Text(s) => s,
                    Cbor::Integer(i) => {
                        let n: i128 = i.into();
                        n.to_string()
                    }
                    other => format!("{other:?}"),
                };
                obj.insert(key, cbor_to_json(v));
            }
            Value::Object(obj)
        }
        Cbor::Tag(_, inner) => cbor_to_json(*inner),
        _ => Value::Null,
    }
}

fn json_to_cbor(value: &Value) -> ciborium::value::Value {
    use ciborium::value::Value as Cbor;
    match value {
        Value::Null => Cbor::Null,
        Value::Bool(b) => Cbor::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Cbor::Integer(i.into())
            } else if let Some(u) = n.as_u64() {
                Cbor::Integer(u.into())
            } else if let Some(f) = n.as_f64() {
                Cbor::Float(f)
            } else {
                Cbor::Null
            }
        }
        Value::String(s) => Cbor::Text(s.clone()),
        Value::Array(arr) => Cbor::Array(arr.iter().map(json_to_cbor).collect()),
        Value::Object(map) => {
            // Check for Buffer type marker
            if map.get("__type").and_then(Value::as_str) == Some("Buffer") {
                if let Some(data) = map.get("data").and_then(Value::as_str) {
                    if let Ok(bytes) = base64_decode(data) {
                        return Cbor::Bytes(bytes);
                    }
                }
            }
            Cbor::Map(
                map.iter()
                    .map(|(k, v)| (Cbor::Text(k.clone()), json_to_cbor(v)))
                    .collect(),
            )
        }
    }
}

/// Public base64 encode for use in bridge call handlers.
pub fn base64_encode_pub(data: &[u8]) -> String {
    base64_encode(data)
}

pub fn base64_decode_pub(input: &str) -> Option<Vec<u8>> {
    base64_decode(input).ok()
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ()> {
    fn decode_char(c: u8) -> Result<u8, ()> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            b'=' => Ok(0),
            _ => Err(()),
        }
    }
    let bytes = input.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        if chunk.len() < 4 {
            return Err(());
        }
        let a = decode_char(chunk[0])?;
        let b = decode_char(chunk[1])?;
        let c = decode_char(chunk[2])?;
        let d = decode_char(chunk[3])?;
        let triple = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);
        result.push((triple >> 16) as u8);
        if chunk[2] != b'=' {
            result.push((triple >> 8) as u8);
        }
        if chunk[3] != b'=' {
            result.push(triple as u8);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{json_to_cbor_payload, map_bridge_method};
    use serde_json::json;

    #[test]
    fn cbor_byte_string_boundary_includes_five_byte_header() {
        let payload_limit = 256 * 1024;
        let raw_bytes = payload_limit - 5;
        let encoded = json_to_cbor_payload(&json!({
            "__type": "Buffer",
            "data": super::base64_encode_pub(&vec![0xA5; raw_bytes]),
        }))
        .expect("encode boundary byte string");
        assert_eq!(encoded.len(), payload_limit);

        let oversized = json_to_cbor_payload(&json!({
            "__type": "Buffer",
            "data": super::base64_encode_pub(&vec![0xA5; raw_bytes + 1]),
        }))
        .expect("encode oversized byte string");
        assert_eq!(oversized.len(), payload_limit + 1);
    }

    #[test]
    fn audited_bridge_methods_map_to_named_handlers() {
        for method in [
            "_cryptoHashDigest",
            "_cryptoSubtle",
            "_networkHttp2ServerListenRaw",
            "_networkHttpServerRequestRaw",
            "_networkHttp2SessionConnectRaw",
            "_networkHttp2StreamRespondRaw",
            "_upgradeSocketWriteRaw",
            "_netSocketSetNoDelayRaw",
            "_kernelStdioWriteRaw",
            "_kernelPollRaw",
            "_kernelFlockRaw",
            "_kernelTtySizeRaw",
            "_netSocketUpgradeTlsRaw",
            "_tlsGetCiphersRaw",
            "_dgramSocketAddressRaw",
            "_dgramSocketSetBufferSizeRaw",
        ] {
            let (mapped, _) = map_bridge_method(method);
            assert_ne!(mapped, method, "missing bridge-method mapping for {method}");
        }
    }

    #[test]
    fn http_request_bridge_shortcut_is_not_mapped() {
        assert_eq!(
            map_bridge_method("_networkHttpRequestRaw"),
            ("_networkHttpRequestRaw", false)
        );
    }
}
