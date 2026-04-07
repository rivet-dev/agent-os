//! V8 runtime host — manages a shared V8 binary process with session multiplexing.
//!
//! One V8 process serves multiple isolate sessions. A reader thread demultiplexes
//! incoming frames to the correct session channel.

use crate::v8_ipc::{self, BinaryFrame};
use std::collections::HashMap;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Environment variable for V8 runtime binary path override.
const V8_RUNTIME_PATH_ENV: &str = "AGENT_OS_V8_RUNTIME_PATH";
/// Default binary name.
const V8_BINARY_NAME: &str = "agent-os-v8";
/// Pre-bundled polyfill bridge code.
const V8_BRIDGE_CODE: &str = include_str!("../assets/v8-bridge.js");

/// Manages a V8 runtime child process with session multiplexing.
pub struct V8RuntimeHost {
    writer: Arc<Mutex<BufWriter<UnixStream>>>,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<BinaryFrame>>>>,
    _child: Child,
    _reader_handle: thread::JoinHandle<()>,
}

impl V8RuntimeHost {
    /// Spawn the V8 runtime binary and set up the demultiplexing reader.
    pub fn spawn() -> io::Result<Self> {
        let binary_path = resolve_v8_binary()?;
        let token = generate_token();

        let mut child = Command::new(&binary_path)
            .env("SECURE_EXEC_V8_TOKEN", &token)
            .env("SECURE_EXEC_V8_CODEC", "cbor")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("failed to spawn V8 runtime at {binary_path}: {e}"),
                )
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "V8 runtime stdout not captured")
        })?;
        let socket_path = read_socket_path(stdout)?;
        let stream = connect_with_retry(&socket_path, Duration::from_secs(5))?;

        // Split into reader and writer
        let reader_stream = stream.try_clone()?;
        let writer = Arc::new(Mutex::new(BufWriter::new(stream)));

        // Authenticate
        {
            let mut w = writer.lock().expect("writer lock");
            let frame_bytes = v8_ipc::encode_frame(&BinaryFrame::Authenticate { token })?;
            w.write_all(&frame_bytes)?;
            w.flush()?;
        }

        // Session demultiplexer
        let sessions: Arc<Mutex<HashMap<String, mpsc::Sender<BinaryFrame>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let sessions_clone = sessions.clone();

        let reader_handle = thread::spawn(move || {
            let mut reader = BufReader::new(reader_stream);
            loop {
                match read_frame(&mut reader) {
                    Ok(frame) => {
                        let session_id = frame_session_id(&frame);
                        if let Some(sid) = session_id {
                            let senders = sessions_clone.lock().expect("sessions lock");
                            if let Some(sender) = senders.get(sid) {
                                let _ = sender.send(frame);
                            }
                        }
                    }
                    Err(e) => {
                        if e.kind() != io::ErrorKind::UnexpectedEof {
                            eprintln!("V8 runtime reader error: {e}");
                        }
                        break;
                    }
                }
            }
        });

        Ok(V8RuntimeHost {
            writer,
            sessions,
            _child: child,
            _reader_handle: reader_handle,
        })
    }

    /// Register a session and return a receiver for its frames.
    pub fn register_session(&self, session_id: &str) -> mpsc::Receiver<BinaryFrame> {
        let (sender, receiver) = mpsc::channel();
        self.sessions
            .lock()
            .expect("sessions lock")
            .insert(session_id.to_owned(), sender);
        receiver
    }

    /// Unregister a session.
    pub fn unregister_session(&self, session_id: &str) {
        self.sessions
            .lock()
            .expect("sessions lock")
            .remove(session_id);
    }

    /// Send a frame to the V8 runtime.
    pub fn send_frame(&self, frame: &BinaryFrame) -> io::Result<()> {
        let bytes = v8_ipc::encode_frame(frame)?;
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(&bytes)?;
        w.flush()
    }

    /// Get the pre-bundled bridge code (polyfills).
    pub fn bridge_code() -> &'static str {
        V8_BRIDGE_CODE
    }

    /// Get a clone of the writer handle for creating session handles.
    pub fn writer_handle(&self) -> Arc<Mutex<BufWriter<UnixStream>>> {
        self.writer.clone()
    }
}

/// A handle to a single V8 session within the shared runtime.
/// Provides methods for sending frames specific to this session.
pub struct V8SessionHandle {
    session_id: String,
    #[allow(clippy::type_complexity)]
    writer: Arc<Mutex<BufWriter<UnixStream>>>,
}

impl std::fmt::Debug for V8SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V8SessionHandle")
            .field("session_id", &self.session_id)
            .finish()
    }
}

impl V8SessionHandle {
    pub fn new(session_id: String, writer: Arc<Mutex<BufWriter<UnixStream>>>) -> Self {
        Self { session_id, writer }
    }

    /// Send a bridge response back to the V8 isolate.
    pub fn send_bridge_response(
        &self,
        call_id: u64,
        status: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        let frame = BinaryFrame::BridgeResponse {
            session_id: self.session_id.clone(),
            call_id,
            status,
            payload,
        };
        let bytes = v8_ipc::encode_frame(&frame)?;
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(&bytes)?;
        w.flush()
    }

    /// Send a stream event to the V8 isolate (stdin data, timer, etc.).
    pub fn send_stream_event(&self, event_type: &str, payload: Vec<u8>) -> io::Result<()> {
        let frame = BinaryFrame::StreamEvent {
            session_id: self.session_id.clone(),
            event_type: event_type.to_owned(),
            payload,
        };
        let bytes = v8_ipc::encode_frame(&frame)?;
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(&bytes)?;
        w.flush()
    }

    /// Terminate execution in this session.
    pub fn terminate(&self) -> io::Result<()> {
        let frame = BinaryFrame::TerminateExecution {
            session_id: self.session_id.clone(),
        };
        let bytes = v8_ipc::encode_frame(&frame)?;
        let mut w = self.writer.lock().expect("writer lock");
        w.write_all(&bytes)?;
        w.flush()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Clone for V8SessionHandle {
    fn clone(&self) -> Self {
        Self {
            session_id: self.session_id.clone(),
            writer: self.writer.clone(),
        }
    }
}

// -- Internal helpers --

fn frame_session_id(frame: &BinaryFrame) -> Option<&str> {
    match frame {
        BinaryFrame::BridgeCall { session_id, .. }
        | BinaryFrame::ExecutionResult { session_id, .. }
        | BinaryFrame::Log { session_id, .. }
        | BinaryFrame::StreamCallback { session_id, .. } => Some(session_id),
        _ => None,
    }
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> io::Result<BinaryFrame> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let total_len = u32::from_be_bytes(len_buf);
    if total_len > 64 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {total_len} exceeds maximum"),
        ));
    }
    let mut buf = vec![0u8; total_len as usize];
    reader.read_exact(&mut buf)?;
    v8_ipc::decode_frame(&buf)
}

fn resolve_v8_binary() -> io::Result<String> {
    if let Ok(path) = std::env::var(V8_RUNTIME_PATH_ENV) {
        if std::path::Path::new(&path).exists() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{V8_RUNTIME_PATH_ENV}={path} does not exist"),
        ));
    }

    if let Ok(exe) = std::env::current_exe() {
        // Check alongside the current executable and in parent directories
        // (handles target/debug/deps/ → target/debug/ for test binaries)
        let mut dir = exe.parent().map(std::path::Path::to_path_buf);
        for _ in 0..3 {
            if let Some(d) = &dir {
                for name in &[V8_BINARY_NAME, "secure-exec-v8"] {
                    let candidate = d.join(name);
                    if candidate.exists() {
                        return Ok(candidate.to_string_lossy().into_owned());
                    }
                }
                dir = d.parent().map(std::path::Path::to_path_buf);
            }
        }
    }

    for profile in &["release", "debug"] {
        let target = format!("target/{profile}/{V8_BINARY_NAME}");
        if std::path::Path::new(&target).exists() {
            return Ok(target);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "V8 runtime binary '{V8_BINARY_NAME}' not found. \
             Set {V8_RUNTIME_PATH_ENV} to specify the path."
        ),
    ))
}

fn generate_token() -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{:032x}{:08x}", seed, std::process::id())
}

fn read_socket_path(stdout: std::process::ChildStdout) -> io::Result<String> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
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
