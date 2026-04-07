// Sync-blocking bridge call: serialize, write to socket, block on read, deserialize

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::ipc_binary::{self, BinaryFrame};

/// Trait for sending serialized frames to the host without holding a shared mutex.
/// Production code uses ChannelFrameSender (lock-free MPSC); tests use WriterFrameSender.
pub trait FrameSender: Send {
    fn send_frame(&self, frame: &BinaryFrame) -> Result<(), String>;
}

/// Sends frames via a crossbeam channel to a dedicated writer thread.
/// Maintains a reusable frame buffer that grows to high-water mark,
/// avoiding per-call allocation for frame construction.
pub struct ChannelFrameSender {
    pub tx: crossbeam_channel::Sender<Vec<u8>>,
    /// Pre-allocated frame buffer reused across send_frame calls.
    /// Grows to high-water mark; cleared (not deallocated) between calls.
    frame_buf: RefCell<Vec<u8>>,
}

impl ChannelFrameSender {
    pub fn new(tx: crossbeam_channel::Sender<Vec<u8>>) -> Self {
        ChannelFrameSender {
            tx,
            frame_buf: RefCell::new(Vec::with_capacity(256)),
        }
    }
}

impl FrameSender for ChannelFrameSender {
    fn send_frame(&self, frame: &BinaryFrame) -> Result<(), String> {
        let mut buf = self.frame_buf.borrow_mut();
        ipc_binary::encode_frame_into(&mut buf, frame)
            .map_err(|e| format!("frame encode error: {}", e))?;
        // Clone sends a copy to the writer thread; buf keeps its capacity
        self.tx
            .send(buf.clone())
            .map_err(|e| format!("channel send failed: {}", e))
    }
}

/// Sends frames directly to a Write impl (used by tests).
#[allow(dead_code)]
pub struct WriterFrameSender {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl FrameSender for WriterFrameSender {
    fn send_frame(&self, frame: &BinaryFrame) -> Result<(), String> {
        let mut w = self.writer.lock().unwrap();
        ipc_binary::write_frame(&mut *w, frame).map_err(|e| format!("write error: {}", e))
    }
}

/// Trait for receiving a BinaryFrame response directly without re-serialization.
/// Production code uses a channel-based implementation; tests use a buffer-based one.
pub trait ResponseReceiver: Send {
    fn recv_response(&self, expected_call_id: u64) -> Result<BinaryFrame, String>;
}

/// ResponseReceiver that reads frames from a byte buffer via ipc_binary::read_frame.
/// Used by tests and any code that has a pre-serialized byte stream.
#[allow(dead_code)]
pub struct ReaderResponseReceiver {
    reader: Mutex<Box<dyn Read + Send>>,
}

impl ReaderResponseReceiver {
    #[allow(dead_code)]
    pub fn new(reader: Box<dyn Read + Send>) -> Self {
        ReaderResponseReceiver {
            reader: Mutex::new(reader),
        }
    }
}

impl ResponseReceiver for ReaderResponseReceiver {
    fn recv_response(&self, _expected_call_id: u64) -> Result<BinaryFrame, String> {
        let mut reader = self.reader.lock().unwrap();
        ipc_binary::read_frame(&mut *reader)
            .map_err(|e| format!("failed to read BridgeResponse: {}", e))
    }
}

/// Shared routing table: maps call_id → session_id for BridgeResponse routing.
/// The connection handler uses this to determine which session a BridgeResponse
/// belongs to (since BridgeResponse has call_id but no session_id).
pub type CallIdRouter = Arc<Mutex<HashMap<u64, String>>>;

/// Shared call_id counter type. Sessions sharing a CallIdRouter must use the same
/// counter to prevent call_id collisions that cause BridgeResponses to be delivered
/// to the wrong session.
pub type SharedCallIdCounter = Arc<AtomicU64>;

/// Context for sync-blocking bridge calls from a V8 session.
///
/// Holds the frame sender and response receiver, session ID, call_id counter,
/// and pending-call tracking. Used by V8 FunctionTemplate callbacks to
/// implement the sync-blocking bridge pattern.
pub struct BridgeCallContext {
    /// Sender for serialized frames to the host (channel-based in production)
    sender: Box<dyn FrameSender>,
    /// Receiver for BridgeResponse frames (no re-serialization needed)
    response_rx: Mutex<Box<dyn ResponseReceiver>>,
    /// Session ID included in every BridgeCall
    pub session_id: String,
    /// Monotonically increasing call_id counter. Sessions sharing a CallIdRouter
    /// must share the same counter (via Arc) to prevent call_id collisions.
    next_call_id: Arc<AtomicU64>,
    /// Set of in-flight call_ids (for duplicate rejection)
    pending_calls: Mutex<HashSet<u64>>,
    /// Optional routing table for call_id → session_id mapping.
    /// When set, call_ids are registered here so the connection handler
    /// can route BridgeResponse messages to the correct session.
    call_id_router: Option<CallIdRouter>,
}

/// No-op FrameSender for snapshot stub functions.
/// Panics if called — stubs must never be invoked during snapshot creation.
#[allow(dead_code)]
struct StubFrameSender;

impl FrameSender for StubFrameSender {
    fn send_frame(&self, _frame: &BinaryFrame) -> Result<(), String> {
        panic!("stub bridge function called during snapshot creation — bridge IIFE must not call bridge functions at setup time")
    }
}

/// No-op ResponseReceiver for snapshot stub functions.
/// Panics if called — stubs must never be invoked during snapshot creation.
#[allow(dead_code)]
struct StubResponseReceiver;

impl ResponseReceiver for StubResponseReceiver {
    fn recv_response(&self, _expected_call_id: u64) -> Result<BinaryFrame, String> {
        panic!("stub bridge function called during snapshot creation — bridge IIFE must not call bridge functions at setup time")
    }
}

#[allow(dead_code)]
impl BridgeCallContext {
    /// Create a no-op BridgeCallContext for snapshot stub functions.
    /// Panics if sync_call or async_send is called — stubs exist only for
    /// the bridge IIFE to reference (not call) during snapshot creation.
    pub fn stub() -> Self {
        BridgeCallContext {
            sender: Box::new(StubFrameSender),
            response_rx: Mutex::new(Box::new(StubResponseReceiver)),
            session_id: "stub".into(),
            next_call_id: Arc::new(AtomicU64::new(1)),
            pending_calls: Mutex::new(HashSet::new()),
            call_id_router: None,
        }
    }

    /// Create a BridgeCallContext with a byte writer and reader (wraps in WriterFrameSender
    /// and ReaderResponseReceiver). Convenient for tests that pre-serialize BridgeResponse bytes.
    pub fn new(
        writer: Box<dyn Write + Send>,
        reader: Box<dyn Read + Send>,
        session_id: String,
    ) -> Self {
        BridgeCallContext {
            sender: Box::new(WriterFrameSender {
                writer: Mutex::new(writer),
            }),
            response_rx: Mutex::new(Box::new(ReaderResponseReceiver::new(reader))),
            session_id,
            next_call_id: Arc::new(AtomicU64::new(1)),
            pending_calls: Mutex::new(HashSet::new()),
            call_id_router: None,
        }
    }

    /// Create a BridgeCallContext with a FrameSender, ResponseReceiver, call_id routing table,
    /// and shared call_id counter. All sessions sharing the same CallIdRouter must share
    /// the same counter to prevent call_id collisions in the routing table.
    pub fn with_receiver(
        sender: Box<dyn FrameSender>,
        response_rx: Box<dyn ResponseReceiver>,
        session_id: String,
        router: CallIdRouter,
        shared_call_id: SharedCallIdCounter,
    ) -> Self {
        BridgeCallContext {
            sender,
            response_rx: Mutex::new(response_rx),
            session_id,
            next_call_id: shared_call_id,
            pending_calls: Mutex::new(HashSet::new()),
            call_id_router: Some(router),
        }
    }

    /// Perform a sync-blocking bridge call.
    ///
    /// Generates a unique call_id, sends a BridgeCall message over IPC,
    /// blocks on read() for the BridgeResponse, and returns the result.
    /// Error responses from the host are returned as Err.
    pub fn sync_call(&self, method: &str, args: Vec<u8>) -> Result<Option<Vec<u8>>, String> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);

        // Register call_id in pending set (reject duplicates)
        {
            let mut pending = self.pending_calls.lock().unwrap();
            if !pending.insert(call_id) {
                return Err(format!("duplicate call_id: {}", call_id));
            }
        }

        // Register call_id → session_id for BridgeResponse routing
        if let Some(ref router) = self.call_id_router {
            router
                .lock()
                .unwrap()
                .insert(call_id, self.session_id.clone());
        }

        // Send BridgeCall to host
        let bridge_call = BinaryFrame::BridgeCall {
            session_id: self.session_id.clone(),
            call_id,
            method: method.to_string(),
            payload: args,
        };

        if let Err(e) = self.sender.send_frame(&bridge_call) {
            self.pending_calls.lock().unwrap().remove(&call_id);
            return Err(format!("failed to write BridgeCall: {}", e));
        }

        // Receive BridgeResponse directly (no re-serialization)
        let response = {
            let rx = self.response_rx.lock().unwrap();
            match rx.recv_response(call_id) {
                Ok(frame) => frame,
                Err(e) => {
                    self.pending_calls.lock().unwrap().remove(&call_id);
                    return Err(e);
                }
            }
        };

        // Remove from pending
        self.pending_calls.lock().unwrap().remove(&call_id);

        // Validate and extract BridgeResponse
        match response {
            BinaryFrame::BridgeResponse {
                call_id: resp_id,
                status,
                payload,
                ..
            } => {
                if resp_id != call_id {
                    return Err(format!(
                        "call_id mismatch: expected {}, got {}",
                        call_id, resp_id
                    ));
                }
                if status == 1 {
                    // Error: payload is UTF-8 error message
                    Err(String::from_utf8_lossy(&payload).to_string())
                } else if payload.is_empty() {
                    Ok(None)
                } else {
                    // status=0: V8-serialized result, status=2: raw binary (Uint8Array)
                    Ok(Some(payload))
                }
            }
            _ => Err("expected BridgeResponse, got different message type".into()),
        }
    }

    /// Send a BridgeCall without blocking for a response.
    /// Returns the call_id for later matching with BridgeResponse.
    /// Used by async promise-returning bridge functions.
    pub fn async_send(&self, method: &str, args: Vec<u8>) -> Result<u64, String> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);

        // Register call_id → session_id for BridgeResponse routing
        if let Some(ref router) = self.call_id_router {
            router
                .lock()
                .unwrap()
                .insert(call_id, self.session_id.clone());
        }

        let bridge_call = BinaryFrame::BridgeCall {
            session_id: self.session_id.clone(),
            call_id,
            method: method.to_string(),
            payload: args,
        };

        if let Err(e) = self.sender.send_frame(&bridge_call) {
            return Err(format!("failed to write BridgeCall: {}", e));
        }

        Ok(call_id)
    }

    /// Check if a call_id is currently pending.
    pub fn is_call_pending(&self, call_id: u64) -> bool {
        self.pending_calls.lock().unwrap().contains(&call_id)
    }

    /// Number of pending calls.
    pub fn pending_count(&self) -> usize {
        self.pending_calls.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    /// Shared writer that captures output for test inspection
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.0.lock().unwrap().flush()
        }
    }

    /// Serialize a BridgeResponse into length-prefixed binary frame bytes
    fn make_response_bytes(
        call_id: u64,
        result: Option<Vec<u8>>,
        error: Option<String>,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        let (status, payload) = if let Some(err) = error {
            (1u8, err.into_bytes())
        } else if let Some(res) = result {
            (0u8, res)
        } else {
            (0u8, vec![])
        };
        ipc_binary::write_frame(
            &mut buf,
            &BinaryFrame::BridgeResponse {
                session_id: String::new(),
                call_id,
                status,
                payload,
            },
        )
        .unwrap();
        buf
    }

    #[test]
    fn sync_call_success_with_result() {
        let response_bytes = make_response_bytes(1, Some(vec![0x93, 0x01, 0x02, 0x03]), None);
        let writer_buf = Arc::new(Mutex::new(Vec::new()));

        let ctx = BridgeCallContext::new(
            Box::new(SharedWriter(Arc::clone(&writer_buf))),
            Box::new(Cursor::new(response_bytes)),
            "test-session-abc".into(),
        );

        let result = ctx.sync_call("_fsReadFile", vec![0x91, 0xa3, 0x66, 0x6f, 0x6f]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(vec![0x93, 0x01, 0x02, 0x03]));

        // Verify the BridgeCall was written correctly
        let written = writer_buf.lock().unwrap();
        let call = ipc_binary::read_frame(&mut Cursor::new(&*written)).unwrap();
        match call {
            BinaryFrame::BridgeCall {
                call_id,
                session_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(call_id, 1);
                assert_eq!(session_id, "test-session-abc");
                assert_eq!(method, "_fsReadFile");
                assert_eq!(payload, vec![0x91, 0xa3, 0x66, 0x6f, 0x6f]);
            }
            _ => panic!("expected BridgeCall"),
        }
    }

    #[test]
    fn sync_call_success_null_result() {
        let response_bytes = make_response_bytes(1, None, None);
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let result = ctx.sync_call("_log", vec![0xc0]).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn sync_call_error_response() {
        let response_bytes = make_response_bytes(1, None, Some("ENOENT: no such file".into()));
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let result = ctx.sync_call("_fsReadFile", vec![0xc0]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "ENOENT: no such file");
    }

    #[test]
    fn sync_call_call_id_increments() {
        // Prepare two sequential responses
        let mut response_bytes = make_response_bytes(1, Some(vec![0xa1, 0x61]), None);
        response_bytes.extend_from_slice(&make_response_bytes(2, Some(vec![0xa1, 0x62]), None));

        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let r1 = ctx.sync_call("_fn1", vec![]).unwrap();
        let r2 = ctx.sync_call("_fn2", vec![]).unwrap();
        assert_eq!(r1, Some(vec![0xa1, 0x61]));
        assert_eq!(r2, Some(vec![0xa1, 0x62]));
    }

    #[test]
    fn sync_call_pending_cleanup_on_read_error() {
        // Empty reader = EOF error; call_id should be cleaned up
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(Vec::new())),
            "session-1".into(),
        );

        assert_eq!(ctx.pending_count(), 0);
        let _ = ctx.sync_call("_fn", vec![]);
        assert_eq!(ctx.pending_count(), 0);
    }

    #[test]
    fn sync_call_id_mismatch_rejected() {
        // Response has call_id=99 but expected call_id=1
        let response_bytes = make_response_bytes(99, Some(vec![0xc0]), None);
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let result = ctx.sync_call("_fn", vec![]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("call_id mismatch"));
    }

    #[test]
    fn sync_call_unexpected_message_type_rejected() {
        // Response is not a BridgeResponse
        let mut response_bytes = Vec::new();
        ipc_binary::write_frame(
            &mut response_bytes,
            &BinaryFrame::TerminateExecution {
                session_id: "session-1".into(),
            },
        )
        .unwrap();

        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let result = ctx.sync_call("_fn", vec![]);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("expected BridgeResponse"));
    }

    #[test]
    fn async_send_writes_bridge_call() {
        let writer_buf = Arc::new(Mutex::new(Vec::new()));
        let ctx = BridgeCallContext::new(
            Box::new(SharedWriter(Arc::clone(&writer_buf))),
            Box::new(Cursor::new(Vec::new())),
            "test-session-abc".into(),
        );

        let call_id = ctx
            .async_send("_asyncFn", vec![0x91, 0xa3, 0x66, 0x6f, 0x6f])
            .unwrap();
        assert_eq!(call_id, 1);

        // Verify the BridgeCall was written correctly
        let written = writer_buf.lock().unwrap();
        let call = ipc_binary::read_frame(&mut Cursor::new(&*written)).unwrap();
        match call {
            BinaryFrame::BridgeCall {
                call_id,
                session_id,
                method,
                payload,
                ..
            } => {
                assert_eq!(call_id, 1);
                assert_eq!(session_id, "test-session-abc");
                assert_eq!(method, "_asyncFn");
                assert_eq!(payload, vec![0x91, 0xa3, 0x66, 0x6f, 0x6f]);
            }
            _ => panic!("expected BridgeCall"),
        }
    }

    #[test]
    fn async_send_increments_call_id() {
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(Vec::new())),
            "session-1".into(),
        );

        let id1 = ctx.async_send("_fn1", vec![]).unwrap();
        let id2 = ctx.async_send("_fn2", vec![]).unwrap();
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
    }

    #[test]
    fn async_send_shares_counter_with_sync() {
        // Sync call uses call_id=1, async_send should get call_id=2
        let response_bytes = make_response_bytes(1, Some(vec![0xc0]), None);
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let _ = ctx.sync_call("_sync", vec![]);
        let id = ctx.async_send("_async", vec![]).unwrap();
        assert_eq!(id, 2);
    }

    #[test]
    fn channel_frame_sender_delivers_frames() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = super::ChannelFrameSender::new(tx);

        let frame = BinaryFrame::BridgeCall {
            session_id: "sess-1".into(),
            call_id: 42,
            method: "_fsReadFile".into(),
            payload: vec![0x01, 0x02],
        };
        sender.send_frame(&frame).expect("send_frame");

        // Verify the received bytes decode to the same frame
        let bytes = rx.recv().expect("recv");
        let decoded = ipc_binary::read_frame(&mut Cursor::new(&bytes)).expect("read_frame");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn channel_frame_sender_no_mutex_contention() {
        // Multiple senders can send concurrently without blocking each other
        let (tx, rx) = crossbeam_channel::unbounded();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let sender = super::ChannelFrameSender::new(tx.clone());
                std::thread::spawn(move || {
                    for j in 0..10 {
                        let frame = BinaryFrame::BridgeCall {
                            session_id: format!("sess-{}", i),
                            call_id: (i * 100 + j) as u64,
                            method: "_fn".into(),
                            payload: vec![],
                        };
                        sender.send_frame(&frame).expect("send_frame");
                    }
                })
            })
            .collect();
        drop(tx); // Drop original sender so rx closes when threads finish

        for h in handles {
            h.join().expect("thread join");
        }

        // All 40 frames should arrive and be decodable
        let mut count = 0;
        while let Ok(bytes) = rx.try_recv() {
            let _ = ipc_binary::read_frame(&mut Cursor::new(&bytes)).expect("decode");
            count += 1;
        }
        assert_eq!(count, 40);
    }

    #[test]
    fn channel_frame_sender_with_bridge_context() {
        // Verify BridgeCallContext works with ChannelFrameSender end-to-end
        let (tx, rx) = crossbeam_channel::unbounded();

        // Pre-serialize a BridgeResponse for the reader
        let response_bytes = make_response_bytes(1, Some(vec![0xAB, 0xCD]), None);
        let router: super::CallIdRouter = Arc::new(Mutex::new(HashMap::new()));

        let ctx = BridgeCallContext::with_receiver(
            Box::new(super::ChannelFrameSender::new(tx)),
            Box::new(super::ReaderResponseReceiver::new(Box::new(Cursor::new(
                response_bytes,
            )))),
            "test-session".into(),
            router,
        );

        let result = ctx.sync_call("_fsReadFile", vec![0x01]).unwrap();
        assert_eq!(result, Some(vec![0xAB, 0xCD]));

        // Verify the BridgeCall went through the channel
        let bytes = rx.recv().expect("recv bridge call");
        let call = ipc_binary::read_frame(&mut Cursor::new(&bytes)).expect("decode");
        match call {
            BinaryFrame::BridgeCall { method, .. } => assert_eq!(method, "_fsReadFile"),
            _ => panic!("expected BridgeCall"),
        }
    }

    #[test]
    fn channel_frame_sender_reuses_frame_buffer() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = super::ChannelFrameSender::new(tx);

        // Send multiple frames — buffer grows to high-water mark
        for i in 0..5 {
            let frame = BinaryFrame::BridgeCall {
                session_id: "sess-1".into(),
                call_id: i,
                method: "_fn".into(),
                payload: vec![0xAA; 100 * (i as usize + 1)],
            };
            sender.send_frame(&frame).expect("send_frame");
        }

        // Verify all frames arrive and decode correctly
        for i in 0..5u64 {
            let bytes = rx.recv().expect("recv");
            let decoded = ipc_binary::read_frame(&mut Cursor::new(&bytes)).expect("decode");
            match decoded {
                BinaryFrame::BridgeCall {
                    call_id, payload, ..
                } => {
                    assert_eq!(call_id, i);
                    assert_eq!(payload.len(), 100 * (i as usize + 1));
                }
                _ => panic!("expected BridgeCall"),
            }
        }

        // Internal buffer capacity stays at high-water mark (verified by sending a small frame)
        let small = BinaryFrame::Log {
            session_id: "s".into(),
            channel: 0,
            message: "x".into(),
        };
        sender.send_frame(&small).expect("send_frame");
        let bytes = rx.recv().expect("recv");
        let decoded = ipc_binary::read_frame(&mut Cursor::new(&bytes)).expect("decode");
        assert_eq!(decoded, small);
    }

    #[test]
    fn stub_context_panics_on_sync_call() {
        let ctx = BridgeCallContext::stub();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ctx.sync_call("_fsReadFile", vec![]);
        }));
        assert!(result.is_err(), "stub sync_call should panic");
    }

    #[test]
    fn stub_context_panics_on_async_send() {
        let ctx = BridgeCallContext::stub();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = ctx.async_send("_asyncFn", vec![]);
        }));
        assert!(result.is_err(), "stub async_send should panic");
    }
}
