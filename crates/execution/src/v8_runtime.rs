//! V8 isolate runtime manager.
//!
//! Spawns the `agent-os-v8` binary, connects over Unix domain socket,
//! and manages V8 isolate sessions for guest JavaScript execution.

use crate::v8_ipc::{self, BinaryFrame};
use serde_json::Value;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Environment variable for V8 runtime binary path override.
const V8_RUNTIME_PATH_ENV: &str = "AGENT_OS_V8_RUNTIME_PATH";

/// Environment variable for CBOR codec mode.
const V8_CODEC_ENV: &str = "SECURE_EXEC_V8_CODEC";

/// Default binary name if not overridden.
const V8_BINARY_NAME: &str = "agent-os-v8";

/// Manages a V8 runtime child process and its UDS connection.
pub struct V8Runtime {
    _child: Child,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    token: String,
    authenticated: bool,
}

impl V8Runtime {
    /// Spawn the V8 runtime binary and connect over UDS.
    pub fn spawn() -> io::Result<Self> {
        let binary_path = resolve_v8_binary()?;
        let token = generate_token();

        let mut child = Command::new(&binary_path)
            .env("SECURE_EXEC_V8_TOKEN", &token)
            .env(V8_CODEC_ENV, "cbor")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to spawn V8 runtime at {}: {e}", binary_path),
                )
            })?;

        // Read socket path from first line of stdout
        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "V8 runtime stdout not captured")
        })?;
        let socket_path = read_socket_path(stdout)?;

        // Connect to UDS with retry (the binary may need a moment)
        let stream = connect_with_retry(&socket_path, Duration::from_secs(5))?;
        let writer = stream.try_clone()?;
        let reader = BufReader::new(stream);

        let mut runtime = V8Runtime {
            _child: child,
            reader,
            writer,
            token,
            authenticated: false,
        };
        runtime.authenticate()?;
        Ok(runtime)
    }

    fn authenticate(&mut self) -> io::Result<()> {
        let frame = BinaryFrame::Authenticate {
            token: self.token.clone(),
        };
        self.send_frame(&frame)?;
        self.authenticated = true;
        Ok(())
    }

    /// Create a new V8 isolate session.
    pub fn create_session(
        &mut self,
        session_id: &str,
        heap_limit_mb: u32,
        cpu_time_limit_ms: u32,
    ) -> io::Result<()> {
        self.send_frame(&BinaryFrame::CreateSession {
            session_id: session_id.to_owned(),
            heap_limit_mb,
            cpu_time_limit_ms,
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

fn resolve_v8_binary() -> io::Result<String> {
    // 1. Check env var override
    if let Ok(path) = std::env::var(V8_RUNTIME_PATH_ENV) {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{V8_RUNTIME_PATH_ENV}={path} does not exist"),
        ));
    }

    // 2. Check alongside the current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join(V8_BINARY_NAME);
            if sibling.exists() {
                return Ok(sibling.to_string_lossy().into_owned());
            }
        }
    }

    // 3. Check cargo target directory (development)
    for profile in &["release", "debug"] {
        let target = format!("target/{profile}/{V8_BINARY_NAME}");
        if std::path::Path::new(&target).exists() {
            return Ok(target);
        }
    }

    // 4. Fall back to PATH
    if let Ok(output) = std::process::Command::new("which")
        .arg(V8_BINARY_NAME)
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "V8 runtime binary '{V8_BINARY_NAME}' not found. Set {V8_RUNTIME_PATH_ENV} to specify the path."
        ),
    ))
}

fn generate_token() -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    // Generate a simple hex token from timestamp + pid (not cryptographic, but
    // only used for process-local IPC authentication)
    format!("{:032x}{:08x}", seed, std::process::id())
}

fn read_socket_path(stdout: std::process::ChildStdout) -> io::Result<String> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    // Read until newline
    loop {
        let mut byte = [0u8; 1];
        match reader.read_exact(&mut byte) {
            Ok(()) => {
                if byte[0] == b'\n' {
                    break;
                }
                line.push(byte[0]);
            }
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("failed to read V8 runtime socket path: {e}"),
                ));
            }
        }
    }
    String::from_utf8(line).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid socket path: {e}"),
        )
    })
}

fn connect_with_retry(socket_path: &str, timeout: Duration) -> io::Result<UnixStream> {
    let start = std::time::Instant::now();
    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(e) if start.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(10));
                if start.elapsed() >= timeout {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("timed out connecting to V8 runtime at {socket_path}: {e}"),
                    ));
                }
            }
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("failed to connect to V8 runtime at {socket_path}: {e}"),
                ));
            }
        }
    }
}

/// Bridge call method name mapping from V8 polyfill names to sidecar sync RPC names.
/// The V8 polyfills use underscore-prefixed camelCase names while the sidecar
/// uses dot-separated category.method names.
pub fn map_bridge_method(method: &str) -> (&str, bool) {
    // Returns (sidecar_method, needs_arg_translation)
    // For methods where the polyfill arg format matches the sidecar format exactly,
    // needs_arg_translation is false.
    match method {
        // Filesystem operations
        "_fsReadFile" => ("fs.readFileSync", false),
        "_fsWriteFile" => ("fs.writeFileSync", false),
        "_fsReadFileBinary" => ("fs.readFileSync", true), // binary variant
        "_fsWriteFileBinary" => ("fs.writeFileSync", true), // binary variant
        "_fsReadDir" => ("fs.readdirSync", false),
        "_fsMkdir" => ("fs.mkdirSync", false),
        "_fsRmdir" => ("fs.rmdirSync", false),
        "_fsExists" => ("fs.existsSync", false),
        "_fsStat" => ("fs.statSync", false),
        "_fsUnlink" => ("fs.unlinkSync", false),
        "_fsRename" => ("fs.renameSync", false),
        "_fsChmod" => ("fs.chmodSync", false),
        "_fsChown" => ("fs.chownSync", false),
        "_fsLink" => ("fs.linkSync", false),
        "_fsSymlink" => ("fs.symlinkSync", false),
        "_fsReadlink" => ("fs.readlinkSync", false),
        "_fsLstat" => ("fs.lstatSync", false),
        "_fsTruncate" => ("fs.truncateSync", false),
        "_fsUtimes" => ("fs.utimesSync", false),

        // Child process operations
        "_childProcessSpawnStart" => ("child_process.spawn", false),
        "_childProcessPoll" => ("child_process.poll", false),
        "_childProcessStdinWrite" => ("child_process.write_stdin", false),
        "_childProcessStdinClose" => ("child_process.close_stdin", false),
        "_childProcessKill" => ("child_process.kill", false),
        "_childProcessSpawnSync" => ("child_process.spawn_sync", false),

        // DNS operations
        "_networkDnsLookupRaw" => ("dns.lookup", false),

        // Console / logging (handled locally, not forwarded to sidecar)
        "_log" | "_error" => ("__log", false),

        // Module loading
        "_resolveModule" | "_resolveModuleSync" => ("__resolve_module", false),
        "_loadFile" | "_loadFileSync" => ("fs.readFileSync", false),
        "_loadPolyfill" => ("__load_polyfill", false),
        "_batchResolveModules" => ("__batch_resolve_modules", false),

        // Crypto operations (handled by the sidecar or locally)
        "_cryptoRandomFill" => ("crypto.randomFill", false),
        "_cryptoRandomUUID" => ("crypto.randomUUID", false),
        "_cryptoHashDigest" => ("crypto.hashDigest", false),
        "_cryptoHmacDigest" => ("crypto.hmacDigest", false),
        "_cryptoPbkdf2" => ("crypto.pbkdf2", false),
        "_cryptoScrypt" => ("crypto.scrypt", false),
        "_cryptoCipheriv" => ("crypto.cipheriv", false),
        "_cryptoDecipheriv" => ("crypto.decipheriv", false),
        "_cryptoCipherivCreate" => ("crypto.cipherivCreate", false),
        "_cryptoCipherivUpdate" => ("crypto.cipherivUpdate", false),
        "_cryptoCipherivFinal" => ("crypto.cipherivFinal", false),
        "_cryptoSign" => ("crypto.sign", false),
        "_cryptoVerify" => ("crypto.verify", false),
        "_cryptoAsymmetricOp" => ("crypto.asymmetricOp", false),
        "_cryptoCreateKeyObject" => ("crypto.createKeyObject", false),
        "_cryptoGenerateKeyPairSync" => ("crypto.generateKeyPairSync", false),
        "_cryptoGenerateKeySync" => ("crypto.generateKeySync", false),
        "_cryptoGeneratePrimeSync" => ("crypto.generatePrimeSync", false),
        "_cryptoDiffieHellman" => ("crypto.diffieHellman", false),
        "_cryptoDiffieHellmanGroup" => ("crypto.diffieHellmanGroup", false),
        "_cryptoDiffieHellmanSessionCreate" => ("crypto.diffieHellmanSessionCreate", false),
        "_cryptoDiffieHellmanSessionCall" => ("crypto.diffieHellmanSessionCall", false),
        "_cryptoSubtle" => ("crypto.subtle", false),

        // Timer scheduling
        "_scheduleTimer" => ("__schedule_timer", false),

        // Stdin
        "_kernelStdinRead" => ("__kernel_stdin_read", false),

        // Network operations
        "_networkFetchRaw" => ("net.fetch", false),
        "_networkHttpRequestRaw" => ("net.http_request", false),
        "_networkHttpServerListenRaw" => ("net.http_listen", false),
        "_networkHttpServerCloseRaw" => ("net.http_close", false),
        "_networkHttpServerRespondRaw" => ("net.http_respond", false),
        "_networkHttpServerWaitRaw" => ("net.http_wait", false),
        "_networkHttp2ServerListenRaw" => ("net.http2_server_listen", false),
        "_networkHttp2ServerCloseRaw" => ("net.http2_server_close", false),
        "_networkHttp2ServerWaitRaw" => ("net.http2_server_wait", false),
        "_networkHttp2SessionConnectRaw" => ("net.http2_session_connect", false),
        "_networkHttp2SessionRequestRaw" => ("net.http2_session_request", false),
        "_networkHttp2SessionSettingsRaw" => ("net.http2_session_settings", false),
        "_networkHttp2SessionSetLocalWindowSizeRaw" => {
            ("net.http2_session_set_local_window_size", false)
        }
        "_networkHttp2SessionGoawayRaw" => ("net.http2_session_goaway", false),
        "_networkHttp2SessionCloseRaw" => ("net.http2_session_close", false),
        "_networkHttp2SessionDestroyRaw" => ("net.http2_session_destroy", false),
        "_networkHttp2SessionWaitRaw" => ("net.http2_session_wait", false),
        "_networkHttp2ServerPollRaw" => ("net.http2_server_poll", false),
        "_networkHttp2SessionPollRaw" => ("net.http2_session_poll", false),
        "_networkHttp2StreamRespondRaw" => ("net.http2_stream_respond", false),
        "_networkHttp2StreamPushStreamRaw" => ("net.http2_stream_push_stream", false),
        "_networkHttp2StreamWriteRaw" => ("net.http2_stream_write", false),
        "_networkHttp2StreamEndRaw" => ("net.http2_stream_end", false),
        "_networkHttp2StreamCloseRaw" => ("net.http2_stream_close", false),
        "_networkHttp2StreamPauseRaw" => ("net.http2_stream_pause", false),
        "_networkHttp2StreamResumeRaw" => ("net.http2_stream_resume", false),
        "_networkHttp2StreamRespondWithFileRaw" => ("net.http2_stream_respond_with_file", false),
        "_networkHttp2ServerRespondRaw" => ("net.http2_server_respond", false),
        "_upgradeSocketWriteRaw" => ("net.upgrade_socket_write", false),
        "_upgradeSocketEndRaw" => ("net.upgrade_socket_end", false),
        "_upgradeSocketDestroyRaw" => ("net.upgrade_socket_destroy", false),
        "_netSocketConnectRaw" => ("net.connect", false),
        "_netSocketWaitConnectRaw" => ("net.socket_wait_connect", false),
        "_netSocketReadRaw" => ("net.socket_read", false),
        "_netSocketSetNoDelayRaw" => ("net.socket_set_no_delay", false),
        "_netSocketSetKeepAliveRaw" => ("net.socket_set_keep_alive", false),
        "_netSocketWriteRaw" => ("net.write", false),
        "_netSocketEndRaw" => ("net.shutdown", false),
        "_netSocketDestroyRaw" => ("net.destroy", false),
        "_netSocketUpgradeTlsRaw" => ("net.socket_upgrade_tls", false),
        "_netSocketGetTlsClientHelloRaw" => ("net.socket_get_tls_client_hello", false),
        "_netSocketTlsQueryRaw" => ("net.socket_tls_query", false),
        "_tlsGetCiphersRaw" => ("tls.get_ciphers", false),
        "_netServerListenRaw" => ("net.listen", false),
        "_netServerAcceptRaw" => ("net.server_accept", false),
        "_netServerCloseRaw" => ("net.server_close", false),

        // Dgram operations
        "_dgramSocketCreateRaw" => ("dgram.createSocket", false),
        "_dgramSocketBindRaw" => ("dgram.bind", false),
        "_dgramSocketRecvRaw" => ("dgram.poll", false),
        "_dgramSocketSendRaw" => ("dgram.send", false),
        "_dgramSocketCloseRaw" => ("dgram.close", false),
        "_dgramSocketAddressRaw" => ("dgram.address", false),
        "_dgramSocketSetBufferSizeRaw" => ("dgram.setBufferSize", false),
        "_dgramSocketGetBufferSizeRaw" => ("dgram.getBufferSize", false),

        // SQLite operations
        "_sqliteConstantsRaw" => ("sqlite.constants", false),
        "_sqliteDatabaseOpenRaw" => ("sqlite.open", false),
        "_sqliteDatabaseCloseRaw" => ("sqlite.close", false),
        "_sqliteDatabaseExecRaw" => ("sqlite.exec", false),
        "_sqliteDatabaseQueryRaw" => ("sqlite.query", false),
        "_sqliteDatabasePrepareRaw" => ("sqlite.prepare", false),
        "_sqliteDatabaseLocationRaw" => ("sqlite.location", false),
        "_sqliteDatabaseCheckpointRaw" => ("sqlite.checkpoint", false),
        "_sqliteStatementRunRaw" => ("sqlite.statement.run", false),
        "_sqliteStatementGetRaw" => ("sqlite.statement.get", false),
        "_sqliteStatementAllRaw" => ("sqlite.statement.all", false),
        "_sqliteStatementColumnsRaw" => ("sqlite.statement.columns", false),
        "_sqliteStatementSetReturnArraysRaw" => ("sqlite.statement.setReturnArrays", false),
        "_sqliteStatementSetReadBigIntsRaw" => ("sqlite.statement.setReadBigInts", false),
        "_sqliteStatementSetAllowBareNamedParametersRaw" => {
            ("sqlite.statement.setAllowBareNamedParameters", false)
        }
        "_sqliteStatementSetAllowUnknownNamedParametersRaw" => {
            ("sqlite.statement.setAllowUnknownNamedParameters", false)
        }
        "_sqliteStatementFinalizeRaw" => ("sqlite.statement.finalize", false),

        // PTY
        "_ptySetRawMode" => ("__pty_set_raw_mode", false),

        // Pass through unknown methods
        _ => (method, false),
    }
}

#[cfg(test)]
mod tests {
    use super::map_bridge_method;

    #[test]
    fn audited_bridge_methods_map_to_named_handlers() {
        for method in [
            "_cryptoHashDigest",
            "_cryptoSubtle",
            "_networkHttp2ServerListenRaw",
            "_networkHttp2SessionConnectRaw",
            "_networkHttp2StreamRespondRaw",
            "_upgradeSocketWriteRaw",
            "_netSocketSetNoDelayRaw",
            "_netSocketUpgradeTlsRaw",
            "_tlsGetCiphersRaw",
            "_dgramSocketAddressRaw",
            "_dgramSocketSetBufferSizeRaw",
        ] {
            let (mapped, _) = map_bridge_method(method);
            assert_ne!(mapped, method, "missing bridge-method mapping for {method}");
        }
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

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
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
