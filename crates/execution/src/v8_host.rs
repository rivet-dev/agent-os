//! V8 runtime host — manages a shared embedded V8 runtime with session multiplexing.

use crate::v8_ipc::{self, BinaryFrame};
use agent_os_v8_runtime::embedded_runtime::{
    shared_embedded_runtime, EmbeddedV8Runtime, EmbeddedV8SessionHandle,
};
use agent_os_v8_runtime::runtime_protocol::{RuntimeCommand, RuntimeEvent};
use std::io::{self, Cursor};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;

/// Pre-bundled polyfill bridge code.
/// Rebuild `crates/execution/assets/v8-bridge.js` before changing this embed.
const V8_BRIDGE_CODE: &str = concat!(
    include_str!("../assets/v8-bridge.js"),
    "\n",
    include_str!("../assets/v8-bridge-zlib.js")
);

/// Manages an embedded V8 runtime with session multiplexing.
pub struct V8RuntimeHost {
    shared: Arc<SharedEmbeddedRuntimeClient>,
}

struct SharedEmbeddedRuntimeClient {
    runtime: Arc<EmbeddedV8Runtime>,
}

impl V8RuntimeHost {
    /// Connect to the process-global embedded V8 runtime client.
    pub fn spawn() -> io::Result<Self> {
        Ok(V8RuntimeHost {
            shared: shared_embedded_runtime_client()?,
        })
    }

    /// Register a session and return a receiver for its frames.
    pub fn register_session(&self, session_id: &str) -> io::Result<mpsc::Receiver<BinaryFrame>> {
        let runtime_receiver = self.shared.runtime.register_session(session_id)?;
        let (sender, receiver) = mpsc::channel();
        let thread_name = format!("agent-os-v8-session-{session_id}");

        thread::Builder::new().name(thread_name).spawn(move || {
            while let Ok(frame) = runtime_receiver.recv() {
                match from_runtime_event(frame) {
                    Ok(frame) => {
                        if sender.send(frame).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        eprintln!("embedded V8 runtime frame conversion error: {error}");
                        break;
                    }
                }
            }
        })?;

        Ok(receiver)
    }

    /// Unregister a session.
    pub fn unregister_session(&self, session_id: &str) {
        self.shared.runtime.unregister_session(session_id);
    }

    /// Send a frame to the V8 runtime.
    pub fn send_frame(&self, frame: &BinaryFrame) -> io::Result<()> {
        self.shared.runtime.dispatch(to_runtime_command(frame)?)
    }

    /// Get the pre-bundled bridge code (polyfills).
    pub fn bridge_code() -> &'static str {
        V8_BRIDGE_CODE
    }

    /// Create a session handle for sending session-scoped frames and cleanup.
    pub fn session_handle(&self, session_id: String) -> V8SessionHandle {
        V8SessionHandle::new(session_id, Arc::clone(&self.shared.runtime))
    }

    pub fn child_pid(&self) -> u32 {
        0
    }

    pub fn is_alive(&mut self) -> io::Result<bool> {
        Ok(self.shared.runtime.is_alive())
    }

    #[cfg(test)]
    fn runtime_ptr(&self) -> usize {
        Arc::as_ptr(&self.shared.runtime) as usize
    }
}

/// A handle to a single V8 session within the shared runtime.
/// Provides methods for sending frames specific to this session.
pub struct V8SessionHandle {
    inner: EmbeddedV8SessionHandle,
}

impl std::fmt::Debug for V8SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("V8SessionHandle")
            .field("session_id", &self.inner.session_id())
            .finish()
    }
}

impl V8SessionHandle {
    pub fn new(session_id: String, runtime: Arc<EmbeddedV8Runtime>) -> Self {
        Self {
            inner: runtime.session_handle(session_id),
        }
    }

    /// Send a bridge response back to the V8 isolate.
    pub fn send_bridge_response(
        &self,
        call_id: u64,
        status: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        self.inner.send_bridge_response(call_id, status, payload)
    }

    /// Send a stream event to the V8 isolate (stdin data, timer, etc.).
    pub fn send_stream_event(&self, event_type: &str, payload: Vec<u8>) -> io::Result<()> {
        self.inner.send_stream_event(event_type, payload)
    }

    /// Terminate execution in this session.
    pub fn terminate(&self) -> io::Result<()> {
        self.inner.terminate()
    }

    /// Destroy this session in the embedded runtime and remove its receiver.
    pub fn destroy(&self) -> io::Result<()> {
        self.inner.terminate()?;
        self.inner.destroy()
    }

    pub fn session_id(&self) -> &str {
        self.inner.session_id()
    }
}

impl Clone for V8SessionHandle {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

fn shared_embedded_runtime_client() -> io::Result<Arc<SharedEmbeddedRuntimeClient>> {
    static SHARED_RUNTIME: OnceLock<Arc<SharedEmbeddedRuntimeClient>> = OnceLock::new();
    static SHARED_RUNTIME_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    if let Some(shared) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(shared));
    }

    let _guard = SHARED_RUNTIME_INIT_LOCK
        .lock()
        .expect("shared embedded runtime init lock poisoned");
    if let Some(shared) = SHARED_RUNTIME.get() {
        return Ok(Arc::clone(shared));
    }

    let shared = Arc::new(SharedEmbeddedRuntimeClient {
        runtime: shared_embedded_runtime()?,
    });
    let _ = SHARED_RUNTIME.set(Arc::clone(&shared));
    Ok(shared)
}

fn to_runtime_command(frame: &BinaryFrame) -> io::Result<RuntimeCommand> {
    let bytes = v8_ipc::encode_frame(frame)?;
    let runtime_frame = agent_os_v8_runtime::ipc_binary::read_frame(&mut Cursor::new(bytes))?;
    RuntimeCommand::try_from(runtime_frame)
}

fn from_runtime_event(event: RuntimeEvent) -> io::Result<BinaryFrame> {
    let frame: agent_os_v8_runtime::ipc_binary::BinaryFrame = event.into();
    let bytes = agent_os_v8_runtime::ipc_binary::frame_to_bytes(&frame)?;
    v8_ipc::decode_frame(&bytes[4..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_SESSION_ID: AtomicU64 = AtomicU64::new(1);

    fn next_session_id() -> String {
        format!(
            "embedded-runtime-host-{}",
            NEXT_TEST_SESSION_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn embedded_runtime_host_reuses_shared_runtime_service() {
        let first = V8RuntimeHost::spawn().expect("spawn V8 runtime host");
        let second = V8RuntimeHost::spawn().expect("spawn V8 runtime host");
        assert_eq!(
            first.runtime_ptr(),
            second.runtime_ptr(),
            "V8 runtime hosts should reuse the same embedded runtime service"
        );
    }

    #[test]
    fn embedded_runtime_host_create_destroy_recycles_session_ids() {
        let host = V8RuntimeHost::spawn().expect("spawn V8 runtime host");
        let session_id = next_session_id();

        let _first_receiver = host
            .register_session(&session_id)
            .expect("register session output");
        host.send_frame(&BinaryFrame::CreateSession {
            session_id: session_id.clone(),
            heap_limit_mb: 0,
            cpu_time_limit_ms: 0,
        })
        .expect("create embedded runtime session");

        let duplicate_error = host
            .send_frame(&BinaryFrame::CreateSession {
                session_id: session_id.clone(),
                heap_limit_mb: 0,
                cpu_time_limit_ms: 0,
            })
            .expect_err("duplicate session ids should be rejected");
        assert_eq!(duplicate_error.kind(), io::ErrorKind::Other);

        host.session_handle(session_id.clone())
            .destroy()
            .expect("destroy embedded runtime session");

        let _second_receiver = host
            .register_session(&session_id)
            .expect("re-register session output");
        host.send_frame(&BinaryFrame::CreateSession {
            session_id: session_id.clone(),
            heap_limit_mb: 0,
            cpu_time_limit_ms: 0,
        })
        .expect("recreate embedded runtime session");

        host.session_handle(session_id)
            .destroy()
            .expect("destroy recreated session");
    }
}
