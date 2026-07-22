// Sync-blocking bridge call: serialize, write to socket, block on read, deserialize

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ipc_binary::{self, BinaryFrame};
use crate::runtime_protocol::{BridgeResponse, RuntimeEvent};
use crate::session::RuntimeEventEnvelope;
use agentos_runtime::accounting::{Reservation, ResourceClass};
use agentos_runtime::RuntimeContext;

// ── Sync bridge-call round-trip latency (opt-in via AGENTOS_SYNCRPC_LAT=1) ──
// Measures the guest-observed cost of one host_call round trip (send + block on
// response). If this is ~ms per call, the embedded-V8 IPC floor is a big part of
// the evaluate-phase VM tax (~50 fs sync RPCs during SDK init). Writes running
// (count, total_us, max_us) to AGENTOS_SYNCRPC_LAT_FILE.
static SYNCRPC_LAT: std::sync::OnceLock<std::sync::Mutex<(u64, u64, u64)>> =
    std::sync::OnceLock::new();

fn syncrpc_lat_enabled() -> bool {
    std::env::var("AGENTOS_SYNCRPC_LAT").as_deref() == Ok("1")
}

fn record_syncrpc_lat(ns: u64) {
    let m = SYNCRPC_LAT.get_or_init(|| std::sync::Mutex::new((0, 0, 0)));
    let Ok(mut a) = m.lock() else {
        eprintln!(
            "ERR_AGENTOS_SYNCRPC_LAT_METRICS_POISONED: sync bridge latency metrics lock poisoned"
        );
        return;
    };
    a.0 = a.0.saturating_add(1);
    a.1 = a.1.saturating_add(ns / 1000);
    a.2 = a.2.max(ns / 1000);
    if a.0 % 25 == 0 {
        if let Ok(path) = std::env::var("AGENTOS_SYNCRPC_LAT_FILE") {
            if let Err(error) = std::fs::write(
                &path,
                format!(
                    "calls={} total_us={} avg_us={} max_us={}\n",
                    a.0,
                    a.1,
                    a.1 / a.0,
                    a.2
                ),
            ) {
                eprintln!(
                    "WARN_AGENTOS_SYNCRPC_LAT_METRICS_WRITE: path={} error={error}",
                    path
                );
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
struct SyncBridgeHostPhaseStats {
    calls: u64,
    total_us: u64,
    max_us: u64,
}

static SYNC_BRIDGE_HOST_PHASES: std::sync::OnceLock<
    std::sync::Mutex<BTreeMap<String, SyncBridgeHostPhaseStats>>,
> = std::sync::OnceLock::new();
static SYNC_BRIDGE_CALL_METHODS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, String>>> =
    std::sync::OnceLock::new();

fn sync_bridge_host_phases_enabled() -> bool {
    std::env::var("AGENTOS_SYNC_BRIDGE_HOST_PHASES").as_deref() == Ok("1")
}

pub(crate) fn record_sync_bridge_host_phase(method: &str, stage: &str, elapsed: Duration) {
    if !sync_bridge_host_phases_enabled() {
        return;
    }
    let stats = SYNC_BRIDGE_HOST_PHASES.get_or_init(|| std::sync::Mutex::new(BTreeMap::new()));
    let Ok(mut stats) = stats.lock() else {
        eprintln!(
            "ERR_AGENTOS_SYNC_BRIDGE_PHASE_METRICS_POISONED: sync bridge phase metrics lock poisoned"
        );
        return;
    };
    let elapsed_us = elapsed.as_micros() as u64;
    let key = format!("{method}:{stage}");
    let entry = stats.entry(key).or_default();
    entry.calls = entry.calls.saturating_add(1);
    entry.total_us = entry.total_us.saturating_add(elapsed_us);
    entry.max_us = entry.max_us.max(elapsed_us);

    if let Ok(path) = std::env::var("AGENTOS_SYNC_BRIDGE_HOST_PHASES_FILE") {
        let mut lines = String::new();
        for (key, value) in stats.iter() {
            let Some((method, stage)) = key.split_once(':') else {
                continue;
            };
            let avg_us = value.total_us.checked_div(value.calls).unwrap_or(0);
            lines.push_str(&format!(
                "method={method} stage={stage} calls={} total_us={} avg_us={} max_us={}\n",
                value.calls, value.total_us, avg_us, value.max_us
            ));
        }
        if let Err(error) = std::fs::write(&path, lines) {
            eprintln!(
                "WARN_AGENTOS_SYNC_BRIDGE_PHASE_METRICS_WRITE: path={} error={error}",
                path
            );
        }
    }
}

fn track_sync_bridge_call_method(call_id: u64, method: &str) {
    if !sync_bridge_host_phases_enabled() {
        return;
    }
    let methods = SYNC_BRIDGE_CALL_METHODS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let Ok(mut methods) = methods.lock() else {
        eprintln!(
            "ERR_AGENTOS_SYNC_BRIDGE_METHOD_TRACKING_POISONED: sync bridge method tracking lock poisoned"
        );
        return;
    };
    if methods.len() > 4096 {
        methods.clear();
    }
    methods.insert(call_id, method.to_owned());
}

fn cleanup_sync_bridge_call_tracking(call_id: u64) {
    if let Some(methods) = SYNC_BRIDGE_CALL_METHODS.get() {
        match methods.lock() {
            Ok(mut methods) => {
                methods.remove(&call_id);
            }
            Err(_) => {
                eprintln!(
                    "ERR_AGENTOS_SYNC_BRIDGE_METHOD_TRACKING_POISONED: could not remove call_id={call_id} from diagnostic tracking"
                );
            }
        }
    }
}

/// Trait for sending serialized frames to the host without holding a shared mutex.
/// Production code uses ChannelRuntimeEventSender (lock-free MPSC); tests use WriterRuntimeEventSender.
pub trait RuntimeEventSender: Send {
    fn send_event(&self, event: RuntimeEvent) -> Result<(), String>;
}

/// Sends frames via a crossbeam channel to a dedicated writer thread.
/// Maintains a reusable frame buffer that grows to high-water mark,
/// avoiding per-call allocation for frame construction.
pub struct ChannelRuntimeEventSender {
    pub tx: crate::session::RuntimeEventSender,
    output_generation: Option<u64>,
    /// Pre-allocated frame buffer reused across send_frame calls.
    /// Grows to high-water mark; cleared (not deallocated) between calls.
    #[allow(dead_code)]
    frame_buf: RefCell<Vec<u8>>,
}

impl ChannelRuntimeEventSender {
    pub fn new(
        tx: impl Into<crate::session::RuntimeEventSender>,
        output_generation: Option<u64>,
    ) -> Self {
        ChannelRuntimeEventSender {
            tx: tx.into(),
            output_generation,
            frame_buf: RefCell::new(Vec::with_capacity(256)),
        }
    }
}

impl RuntimeEventSender for ChannelRuntimeEventSender {
    fn send_event(&self, event: RuntimeEvent) -> Result<(), String> {
        self.tx
            .send(RuntimeEventEnvelope {
                output_generation: self.output_generation,
                event,
            })
            .map_err(|error| format!("runtime event send failed: {error}"))
    }
}

/// Sends frames directly to a Write impl (used by tests).
#[allow(dead_code)]
pub struct WriterRuntimeEventSender {
    writer: Mutex<Box<dyn Write + Send>>,
}

impl RuntimeEventSender for WriterRuntimeEventSender {
    fn send_event(&self, event: RuntimeEvent) -> Result<(), String> {
        let mut w = self.writer.lock().unwrap();
        let frame: BinaryFrame = event.into();
        ipc_binary::write_frame(&mut *w, &frame).map_err(|e| format!("write error: {}", e))
    }
}

/// Trait for receiving a BridgeResponse directly without re-serialization.
/// Production code uses a channel-based implementation; tests use a buffer-based one.
pub trait BridgeResponseReceiver: Send {
    fn recv_response(&self, expected_call_id: u64) -> Result<BridgeResponse, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncBridgeCallResponse {
    pub status: u8,
    pub payload: Vec<u8>,
    pub reservation: Option<agentos_runtime::accounting::SharedReservation>,
}

/// ResponseReceiver that reads frames from a byte buffer via ipc_binary::read_frame.
/// Used by tests and any code that has a pre-serialized byte stream.
#[allow(dead_code)]
pub struct ReaderBridgeResponseReceiver {
    reader: Mutex<Box<dyn Read + Send>>,
}

impl ReaderBridgeResponseReceiver {
    #[allow(dead_code)]
    pub fn new(reader: Box<dyn Read + Send>) -> Self {
        ReaderBridgeResponseReceiver {
            reader: Mutex::new(reader),
        }
    }
}

impl BridgeResponseReceiver for ReaderBridgeResponseReceiver {
    fn recv_response(&self, expected_call_id: u64) -> Result<BridgeResponse, String> {
        let mut reader = self.reader.lock().unwrap();
        let frame = ipc_binary::read_frame(&mut *reader)
            .map_err(|e| format!("failed to read BridgeResponse: {}", e))?;
        match frame {
            BinaryFrame::BridgeResponse {
                call_id,
                status,
                payload,
                ..
            } => {
                if call_id != expected_call_id {
                    return Err(format!(
                        "call_id mismatch: expected {}, got {}",
                        expected_call_id, call_id
                    ));
                }
                Ok(BridgeResponse {
                    call_id,
                    status,
                    payload,
                    reservation: None,
                })
            }
            _ => Err("expected BridgeResponse, got different message type".into()),
        }
    }
}

const MAX_PENDING_BRIDGE_CALLS: usize = 16_384;
pub(crate) const DEFAULT_BRIDGE_CALL_TIMEOUT: Duration = Duration::from_secs(30);

fn bridge_call_uses_session_lifetime(method: &str) -> bool {
    // This read is the durable readiness wait behind Node's stdin stream. It
    // can legitimately remain pending for the entire process lifetime and is
    // canceled by session/process teardown. Admission and byte limits still
    // bound the route while it is pending.
    method == "_kernelStdinRead"
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BridgeCallTargetKind {
    Sync,
    Async,
}

struct BridgeCallTarget {
    session_id: String,
    session_generation: Option<u64>,
    kind: BridgeCallTargetKind,
    sender: crossbeam_channel::Sender<BridgeResponse>,
    _call_reservation: Reservation,
    _request_reservation: Reservation,
    response_resources: Arc<agentos_runtime::accounting::ResourceLedger>,
    response_reservation: Reservation,
    max_response_bytes: usize,
    deadline: Option<Instant>,
    timeout: Option<Duration>,
    _deadline_cancellation: Option<tokio::sync::oneshot::Sender<()>>,
    host_visible: bool,
}

const BRIDGE_TERMINAL_RESPONSE_RESERVATION_BYTES: usize = 4 * 1024;

fn grow_response_reservation(
    target: &mut BridgeCallTarget,
    payload_bytes: usize,
) -> Result<(), String> {
    let additional = payload_bytes.saturating_sub(target.response_reservation.amount());
    if additional == 0 {
        return Ok(());
    }
    let extra = target
        .response_resources
        .reserve(ResourceClass::BridgeResponseBytes, additional)
        .map_err(|error| error.to_string())?;
    target.response_reservation.merge(extra).map_err(|_| {
        String::from(
            "ERR_AGENTOS_BRIDGE_RESPONSE_ACCOUNTING: failed to merge response-byte reservations",
        )
    })
}

fn transfer_response_reservation(
    mut reservation: Reservation,
    payload_bytes: usize,
) -> agentos_runtime::accounting::SharedReservation {
    let unused = reservation
        .amount()
        .checked_sub(payload_bytes)
        .expect("response payload must fit its admission reservation");
    if unused != 0 {
        drop(
            reservation
                .split(unused)
                .expect("unused response capacity must remain transferable"),
        );
    }
    agentos_runtime::accounting::SharedReservation::new(reservation)
}

fn encode_terminal_error_payload(code: &str, message: &str, maximum_bytes: usize) -> Vec<u8> {
    let encode = |message: &str| {
        let value = ciborium::Value::Map(vec![
            (
                ciborium::Value::Text(String::from("code")),
                ciborium::Value::Text(code.to_owned()),
            ),
            (
                ciborium::Value::Text(String::from("message")),
                ciborium::Value::Text(message.to_owned()),
            ),
        ]);
        let mut payload = Vec::new();
        ciborium::into_writer(&value, &mut payload)
            .expect("a bridge error map containing only strings must encode");
        payload
    };

    let payload = encode(message);
    if payload.len() <= maximum_bytes {
        return payload;
    }

    // Production bridge declarations reserve at least 4 KiB. For smaller
    // synthetic limits, shorten the diagnostic without ever deriving the
    // stable code from that diagnostic.
    let mut boundary = message.len().min(maximum_bytes);
    while boundary > 0 && !message.is_char_boundary(boundary) {
        boundary -= 1;
    }
    while boundary > 0 {
        let payload = encode(&message[..boundary]);
        if payload.len() <= maximum_bytes {
            return payload;
        }
        boundary -= 1;
        while boundary > 0 && !message.is_char_boundary(boundary) {
            boundary -= 1;
        }
    }
    let payload = encode("");
    if payload.len() <= maximum_bytes {
        payload
    } else {
        Vec::new()
    }
}

fn bridge_error_payload_message(payload: &[u8]) -> String {
    let Ok(ciborium::Value::Map(entries)) = ciborium::from_reader::<ciborium::Value, _>(payload)
    else {
        return String::from_utf8_lossy(payload).to_string();
    };
    let mut code = None;
    let mut message = None;
    for (key, value) in entries {
        let (ciborium::Value::Text(key), ciborium::Value::Text(value)) = (key, value) else {
            continue;
        };
        match key.as_str() {
            "code" => code = Some(value),
            "message" => message = Some(value),
            _ => {}
        }
    }
    match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (_, Some(message)) => message,
        _ => String::from_utf8_lossy(payload).to_string(),
    }
}

fn bridge_call_timeout_message(call_id: u64, timeout: Duration) -> String {
    format!(
        "bridge call_id {call_id} exceeded its {} ms deadline; raise limits.reactor.operationDeadlineMs",
        timeout.as_millis()
    )
}

fn deliver_bridge_call_timeout(call_id: u64, target: BridgeCallTarget) -> Result<String, String> {
    let timeout = target
        .timeout
        .expect("only operation-deadline bridge calls can time out");
    let message = bridge_call_timeout_message(call_id, timeout);
    let error = format!("ERR_AGENTOS_BRIDGE_CALL_TIMEOUT: {message}");
    let payload = encode_terminal_error_payload(
        "ERR_AGENTOS_BRIDGE_CALL_TIMEOUT",
        &message,
        target
            .max_response_bytes
            .min(target.response_reservation.amount()),
    );
    let reservation = transfer_response_reservation(target.response_reservation, payload.len());
    target
        .sender
        .try_send(BridgeResponse {
            call_id,
            status: 1,
            payload,
            reservation: Some(reservation),
        })
        .map_err(|delivery_error| {
            format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: failed to deliver timeout for call_id {call_id}: {delivery_error}; original error: {error}"
            )
        })?;
    Ok(error)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetiredBridgeCall {
    session_id: String,
    session_generation: Option<u64>,
    retirement_epoch: u64,
}

#[derive(Default)]
struct RetiredBridgeCalls {
    by_call_id: HashMap<u64, RetiredBridgeCall>,
    order: VecDeque<(u64, u64)>,
    next_epoch: u64,
}

impl RetiredBridgeCalls {
    fn insert(&mut self, call_id: u64, target: &BridgeCallTarget, limit: usize) {
        self.insert_identity(
            call_id,
            &target.session_id,
            target.session_generation,
            limit,
        );
    }

    fn insert_identity(
        &mut self,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        limit: usize,
    ) {
        while self.order.len() >= limit {
            let Some((oldest_call_id, oldest_epoch)) = self.order.pop_front() else {
                break;
            };
            if self
                .by_call_id
                .get(&oldest_call_id)
                .is_some_and(|retired| retired.retirement_epoch == oldest_epoch)
            {
                self.by_call_id.remove(&oldest_call_id);
            }
        }

        self.next_epoch = self.next_epoch.wrapping_add(1);
        let retirement_epoch = self.next_epoch;
        self.by_call_id.insert(
            call_id,
            RetiredBridgeCall {
                session_id: session_id.to_owned(),
                session_generation,
                retirement_epoch,
            },
        );
        self.order.push_back((call_id, retirement_epoch));
    }

    fn remove(&mut self, call_id: u64) {
        self.by_call_id.remove(&call_id);
    }
}

/// Bounded call-specific response registry shared by all sessions.
///
/// A response is settled directly into the target registered for its globally
/// unique call ID. It never enters the ordinary session command/event channel.
pub struct BridgeCallRegistry {
    pending: Mutex<HashMap<u64, BridgeCallTarget>>,
    /// Bounded proof that a host-visible call was canceled before settlement.
    /// This distinguishes expected teardown completions from arbitrary unknown
    /// or duplicate responses without keeping retired calls forever.
    retired: Mutex<RetiredBridgeCalls>,
    max_pending: usize,
}

/// A bridge-response settlement failure whose classification survives the
/// in-process `io::Error` transport. Callers must branch on `kind`, never on
/// the diagnostic text: the latter may contain guest-controlled values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeSettlementErrorKind {
    StaleCompletion,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeSettlementError {
    kind: BridgeSettlementErrorKind,
    message: String,
}

impl BridgeSettlementError {
    pub fn stale_completion(message: impl Into<String>) -> Self {
        Self {
            kind: BridgeSettlementErrorKind::StaleCompletion,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> BridgeSettlementErrorKind {
        self.kind
    }

    /// Retained for diagnostic assertions; production classification uses
    /// `kind()`.
    pub fn contains(&self, needle: &str) -> bool {
        self.message.contains(needle)
    }
}

impl From<String> for BridgeSettlementError {
    fn from(message: String) -> Self {
        Self {
            kind: BridgeSettlementErrorKind::Other,
            message,
        }
    }
}

impl std::fmt::Display for BridgeSettlementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BridgeSettlementError {}

impl BridgeCallRegistry {
    pub fn new(max_pending: usize) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            retired: Mutex::new(RetiredBridgeCalls::default()),
            max_pending: max_pending.max(1),
        }
    }

    pub fn with_default_limit() -> Self {
        Self::new(MAX_PENDING_BRIDGE_CALLS)
    }

    #[allow(clippy::too_many_arguments)] // one immutable identity/admission tuple per call route
    fn register(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        kind: BridgeCallTargetKind,
        sender: crossbeam_channel::Sender<BridgeResponse>,
        timeout: Option<Duration>,
    ) -> Result<tokio::sync::oneshot::Receiver<()>, String> {
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(String::from(
                "ERR_AGENTOS_BRIDGE_CALL_TIMEOUT_INVALID: limits.reactor.operationDeadlineMs must be greater than zero",
            ));
        }
        let call_reservation = runtime
            .resources()
            .reserve(ResourceClass::BridgeCalls, 1)
            .map_err(|error| error.to_string())?;
        let request_reservation = runtime
            .resources()
            .reserve(ResourceClass::BridgeRequestBytes, request_bytes)
            .map_err(|error| error.to_string())?;
        let configured_response_limit = runtime
            .resources()
            .usage(ResourceClass::BridgeResponseBytes)
            .limit
            .ok_or_else(|| {
                String::from(
                    "ERR_AGENTOS_RESOURCE_UNBOUNDED: bridge response bytes require limits.reactor.maxBridgeResponseBytes",
                )
            })?;
        if max_response_bytes > configured_response_limit {
            return Err(format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT: declared response maximum of {max_response_bytes} bytes exceeds configured limit of {configured_response_limit}; raise limits.reactor.maxBridgeResponseBytes"
            ));
        }
        // Reserve enough capacity to guarantee that every admitted call can
        // settle with a typed terminal error. Reserving the full per-method
        // maximum here would make one 16 MiB-capable call consume the entire
        // default VM budget and reject unrelated concurrent calls even when all
        // concrete responses are tiny. Settlement grows this reservation to the
        // actual payload size before it can enter the response lane.
        let admission_response_bytes =
            max_response_bytes.min(BRIDGE_TERMINAL_RESPONSE_RESERVATION_BYTES);
        let response_reservation = runtime
            .resources()
            .reserve(ResourceClass::BridgeResponseBytes, admission_response_bytes)
            .map_err(|error| error.to_string())?;
        let mut pending = self.pending.lock().map_err(|_| {
            String::from("ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: bridge registry lock poisoned")
        })?;
        if pending.len() >= self.max_pending {
            return Err(format!(
                "ERR_AGENTOS_BRIDGE_CALL_LIMIT: bridge call registry exceeded limit of {} pending calls; raise runtime.resources.maxBridgeCalls",
                self.max_pending
            ));
        }
        if kind == BridgeCallTargetKind::Async {
            let response_capacity = sender.capacity().ok_or_else(|| {
                String::from(
                    "ERR_AGENTOS_BRIDGE_RESPONSE_LANE_UNBOUNDED: async bridge response lanes must be bounded",
                )
            })?;
            let queued_responses = sender.len();
            let registered_targets = pending
                .values()
                .filter(|target| {
                    target.kind == BridgeCallTargetKind::Async
                        && target.sender.same_channel(&sender)
                })
                .count();
            let occupied = queued_responses.saturating_add(registered_targets);
            if occupied >= response_capacity {
                return Err(format!(
                    "ERR_AGENTOS_BRIDGE_RESPONSE_LANE_LIMIT: async response lane has {queued_responses} queued responses and {registered_targets} registered calls at capacity {response_capacity}; admission must reserve a response slot before host visibility"
                ));
            }
        }
        if pending.contains_key(&call_id) {
            return Err(format!(
                "ERR_AGENTOS_BRIDGE_DUPLICATE_CALL_ID: duplicate bridge call_id {call_id}"
            ));
        }
        self.retired
            .lock()
            .map_err(|_| {
                String::from(
                    "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: retired bridge registry lock poisoned",
                )
            })?
            .remove(call_id);
        let (deadline_cancellation, deadline_cancelled) = tokio::sync::oneshot::channel();
        let deadline = timeout
            .map(|timeout| {
                Instant::now().checked_add(timeout).ok_or_else(|| {
                    String::from(
                        "ERR_AGENTOS_BRIDGE_CALL_TIMEOUT_INVALID: limits.reactor.operationDeadlineMs exceeds the host clock range",
                    )
                })
            })
            .transpose()?;
        pending.insert(
            call_id,
            BridgeCallTarget {
                session_id: session_id.to_owned(),
                session_generation,
                kind,
                sender,
                _call_reservation: call_reservation,
                _request_reservation: request_reservation,
                response_resources: Arc::clone(runtime.resources()),
                response_reservation,
                max_response_bytes,
                deadline,
                timeout,
                _deadline_cancellation: Some(deadline_cancellation),
                host_visible: false,
            },
        );
        Ok(deadline_cancelled)
    }

    /// Marks the point after which cancellation can race a legitimate host
    /// response. Callers invoke this immediately before publishing the event.
    fn mark_host_visible(&self, call_id: u64) -> Result<(), String> {
        let mut pending = self.pending.lock().map_err(|_| {
            String::from("ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: bridge registry lock poisoned")
        })?;
        let target = pending.get_mut(&call_id).ok_or_else(|| {
            format!(
                "ERR_AGENTOS_BRIDGE_ROUTE_RETIRED: bridge call_id {call_id} was canceled before host publication"
            )
        })?;
        target.host_visible = true;
        Ok(())
    }

    pub fn register_sync(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
    ) -> Result<crossbeam_channel::Receiver<BridgeResponse>, String> {
        self.register_sync_with_timeout(
            runtime,
            request_bytes,
            max_response_bytes,
            call_id,
            session_id,
            session_generation,
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn register_sync_with_timeout(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        timeout: Duration,
    ) -> Result<crossbeam_channel::Receiver<BridgeResponse>, String> {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        let _deadline_cancelled = self.register(
            runtime,
            request_bytes,
            max_response_bytes,
            call_id,
            session_id,
            session_generation,
            BridgeCallTargetKind::Sync,
            sender,
            Some(timeout),
        )?;
        Ok(receiver)
    }

    #[allow(clippy::too_many_arguments)] // immutable identity/admission tuple for one direct route
    pub fn register_async(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        sender: crossbeam_channel::Sender<BridgeResponse>,
    ) -> Result<(), String> {
        self.register(
            runtime,
            request_bytes,
            max_response_bytes,
            call_id,
            session_id,
            session_generation,
            BridgeCallTargetKind::Async,
            sender,
            Some(DEFAULT_BRIDGE_CALL_TIMEOUT),
        )
        .map(drop)
    }

    #[allow(clippy::too_many_arguments)]
    fn register_async_with_timeout(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        sender: crossbeam_channel::Sender<BridgeResponse>,
        timeout: Duration,
    ) -> Result<tokio::sync::oneshot::Receiver<()>, String> {
        self.register(
            runtime,
            request_bytes,
            max_response_bytes,
            call_id,
            session_id,
            session_generation,
            BridgeCallTargetKind::Async,
            sender,
            Some(timeout),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn register_async_for_session_lifetime(
        &self,
        runtime: &RuntimeContext,
        request_bytes: usize,
        max_response_bytes: usize,
        call_id: u64,
        session_id: &str,
        session_generation: Option<u64>,
        sender: crossbeam_channel::Sender<BridgeResponse>,
    ) -> Result<(), String> {
        self.register(
            runtime,
            request_bytes,
            max_response_bytes,
            call_id,
            session_id,
            session_generation,
            BridgeCallTargetKind::Async,
            sender,
            None,
        )
        .map(drop)
    }

    fn timeout(&self, call_id: u64) -> Result<bool, String> {
        let mut pending = self.pending.lock().map_err(|_| {
            String::from("ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: bridge registry lock poisoned")
        })?;
        if pending
            .get(&call_id)
            .and_then(|target| target.deadline)
            .is_none_or(|deadline| Instant::now() < deadline)
        {
            return Ok(false);
        }
        let target = pending
            .remove(&call_id)
            .expect("expired bridge target must remain registered while locked");
        if target.host_visible {
            self.retired
                .lock()
                .map_err(|_| {
                    String::from(
                        "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: retired bridge registry lock poisoned",
                    )
                })?
                .insert(call_id, &target, self.max_pending);
        }
        deliver_bridge_call_timeout(call_id, target)?;
        Ok(true)
    }

    pub fn settle(
        &self,
        supplied_session_id: &str,
        supplied_generation: Option<u64>,
        mut response: BridgeResponse,
    ) -> Result<(), BridgeSettlementError> {
        let call_id = response.call_id;
        let mut pending = self.pending.lock().map_err(|_| {
            String::from("ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: bridge registry lock poisoned")
        })?;
        let target = match pending.get(&response.call_id) {
            Some(target) => target,
            None => {
                let retired = self.retired.lock().map_err(|_| {
                    String::from(
                        "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: retired bridge registry lock poisoned",
                    )
                })?;
                if let Some(retired) = retired.by_call_id.get(&response.call_id) {
                    if retired.session_id == supplied_session_id
                        && retired.session_generation == supplied_generation
                    {
                        return Err(BridgeSettlementError::stale_completion(format!(
                            "ERR_AGENTOS_BRIDGE_STALE_COMPLETION: response for canceled host-visible bridge call_id {} in session {} generation {:?}",
                            response.call_id, supplied_session_id, supplied_generation
                        )));
                    }
                    return Err(format!(
                        "ERR_AGENTOS_BRIDGE_STALE_GENERATION: response call_id {} named session {} generation {:?}, expected {} generation {:?}",
                        response.call_id,
                        supplied_session_id,
                        supplied_generation,
                        retired.session_id,
                        retired.session_generation
                    )
                    .into());
                }
                return Err(format!(
                    "ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID: response for unknown bridge call_id {}",
                    response.call_id
                )
                .into());
            }
        };
        if target.session_id != supplied_session_id {
            return Err(format!(
                "ERR_AGENTOS_BRIDGE_STALE_GENERATION: response call_id {} named session {}, expected {}",
                response.call_id, supplied_session_id, target.session_id
            )
            .into());
        }
        if target.session_generation != supplied_generation {
            return Err(format!(
                "ERR_AGENTOS_BRIDGE_STALE_GENERATION: response call_id {} generation {:?}, expected {:?}",
                response.call_id, supplied_generation, target.session_generation
            )
            .into());
        }
        // Identity validation must not consume a legitimate route when a stale
        // response arrives. Once identity is validated, however, settlement is
        // terminal: take the target before attempting any delivery so every
        // success or failure path drops or transfers its call, request, and
        // response reservations exactly once. Keep the registry lock through
        // delivery so an async response lane's registered slot cannot be
        // re-admitted in the gap between the take and try_send.
        let deadline_expired = target
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline);
        let mut target = pending
            .remove(&call_id)
            .expect("validated bridge target must remain registered while locked");

        if deadline_expired {
            if target.host_visible {
                self.retired
                    .lock()
                    .map_err(|_| {
                        String::from(
                            "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: retired bridge registry lock poisoned",
                        )
                    })?
                    .insert(call_id, &target, self.max_pending);
            }
            let error = deliver_bridge_call_timeout(call_id, target)?;
            return Err(error.into());
        }

        if response.payload.len() > target.max_response_bytes {
            let error = format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT: response call_id {} contains {} bytes, exceeding its declared maximum of {}; raise limits.reactor.maxBridgeResponseBytes",
                response.call_id,
                response.payload.len(),
                target.max_response_bytes
            );
            let payload = encode_terminal_error_payload(
                "ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT",
                &format!(
                    "response call_id {} contains {} bytes, exceeding its declared maximum of {}; raise limits.reactor.maxBridgeResponseBytes",
                    response.call_id,
                    response.payload.len(),
                    target.max_response_bytes
                ),
                target
                    .max_response_bytes
                    .min(target.response_reservation.amount()),
            );
            let reservation =
                transfer_response_reservation(target.response_reservation, payload.len());
            target
                .sender
                .try_send(BridgeResponse {
                    call_id: response.call_id,
                    status: 1,
                    payload,
                    reservation: Some(reservation),
                })
                .map_err(|delivery_error| {
                    format!(
                        "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: failed to deliver oversize response error for call_id {}: {delivery_error}; original error: {error}",
                        response.call_id
                    )
                })?;
            return Err(error.into());
        }

        if let Some(reservation) = response.reservation.take() {
            let error = format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_ACCOUNTING: response call_id {} carries a producer-side {:?} reservation of {} bytes; bridge response ownership must come from the call's admission reservation",
                response.call_id,
                reservation.resource(),
                reservation.amount()
            );
            let payload = encode_terminal_error_payload(
                "ERR_AGENTOS_BRIDGE_RESPONSE_ACCOUNTING",
                &format!(
                    "response call_id {} carries a producer-side reservation; bridge response ownership must come from the call's admission reservation",
                    response.call_id
                ),
                target
                    .max_response_bytes
                    .min(target.response_reservation.amount()),
            );
            let response_reservation =
                transfer_response_reservation(target.response_reservation, payload.len());
            target
                .sender
                .try_send(BridgeResponse {
                    call_id: response.call_id,
                    status: 1,
                    payload,
                    reservation: Some(response_reservation),
                })
                .map_err(|delivery_error| {
                    format!(
                        "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: failed to deliver accounting error for call_id {}: {delivery_error}; original error: {error}",
                        response.call_id
                    )
                })?;
            return Err(error.into());
        }

        if let Err(limit_error) = grow_response_reservation(&mut target, response.payload.len()) {
            let error = format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT: response call_id {} could not reserve {} concrete bytes: {}; raise limits.reactor.maxBridgeResponseBytes",
                response.call_id,
                response.payload.len(),
                limit_error
            );
            let payload = encode_terminal_error_payload(
                "ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT",
                &format!(
                    "response call_id {} could not reserve {} concrete bytes; raise limits.reactor.maxBridgeResponseBytes",
                    response.call_id,
                    response.payload.len()
                ),
                target
                    .max_response_bytes
                    .min(target.response_reservation.amount()),
            );
            let response_reservation =
                transfer_response_reservation(target.response_reservation, payload.len());
            target
                .sender
                .try_send(BridgeResponse {
                    call_id: response.call_id,
                    status: 1,
                    payload,
                    reservation: Some(response_reservation),
                })
                .map_err(|delivery_error| {
                    format!(
                        "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: failed to deliver response-capacity error for call_id {}: {delivery_error}; original error: {error}",
                        response.call_id
                    )
                })?;
            return Err(error.into());
        }

        response.reservation = Some(transfer_response_reservation(
            target.response_reservation,
            response.payload.len(),
        ));

        match target.sender.try_send(response) {
            Ok(()) => Ok(()),
            Err(crossbeam_channel::TrySendError::Disconnected(_)) if target.host_visible => {
                self.retired
                    .lock()
                    .map_err(|_| {
                        String::from(
                            "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: retired bridge registry lock poisoned",
                        )
                    })?
                    .insert_identity(
                        call_id,
                        &target.session_id,
                        target.session_generation,
                        self.max_pending,
                    );
                Err(BridgeSettlementError::stale_completion(format!(
                    "ERR_AGENTOS_BRIDGE_STALE_COMPLETION: published {:?} response target disconnected before settlement for call_id {} in session {} generation {:?}",
                    target.kind, call_id, target.session_id, target.session_generation
                )))
            }
            Err(error) => Err(format!(
                "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: {:?} response target for session {} generation {:?} rejected settlement: {error}",
                target.kind, target.session_id, target.session_generation
            )
            .into()),
        }
    }

    fn cancel_with_visibility(&self, call_id: u64, force_unpublished: bool) {
        match self.pending.lock() {
            Ok(mut pending) => {
                let target = pending.remove(&call_id);
                match self.retired.lock() {
                    Ok(mut retired) => {
                        if force_unpublished {
                            // A session cancellation can race between marking
                            // the route and the failed event send. Retract any
                            // conservative tombstone once publication is known
                            // to have failed.
                            retired.remove(call_id);
                        } else if let Some(target) =
                            target.as_ref().filter(|target| target.host_visible)
                        {
                            retired.insert(call_id, target, self.max_pending);
                        }
                    }
                    Err(_) => eprintln!(
                        "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not update retirement for bridge call_id {call_id}"
                    ),
                }
            }
            Err(_) => eprintln!(
                "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not cancel bridge call_id {call_id}"
            ),
        }
    }

    pub fn cancel(&self, call_id: u64) {
        self.cancel_with_visibility(call_id, false);
    }

    fn cancel_unpublished(&self, call_id: u64) {
        self.cancel_with_visibility(call_id, true);
    }

    pub fn cancel_session(&self, session_id: &str, session_generation: Option<u64>) {
        match self.pending.lock() {
            Ok(mut pending) => {
                let canceled_call_ids = pending
                    .iter()
                    .filter_map(|(call_id, target)| {
                        (target.session_id == session_id
                            && (session_generation.is_none()
                                || target.session_generation == session_generation))
                        .then_some(*call_id)
                    })
                    .collect::<Vec<_>>();
                let mut retired = match self.retired.lock() {
                    Ok(retired) => retired,
                    Err(_) => {
                        eprintln!(
                            "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not retire bridge calls for session {session_id} generation {session_generation:?}"
                        );
                        pending.retain(|_, target| {
                            target.session_id != session_id
                                || (session_generation.is_some()
                                    && target.session_generation != session_generation)
                        });
                        return;
                    }
                };
                for call_id in canceled_call_ids {
                    if let Some(target) = pending.remove(&call_id) {
                        if target.host_visible {
                            retired.insert(call_id, &target, self.max_pending);
                        }
                    }
                }
            }
            Err(_) => eprintln!(
                "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not cancel bridge calls for session {session_id} generation {session_generation:?}"
            ),
        }
    }

    pub fn clear(&self) {
        match self.pending.lock() {
            Ok(mut pending) => match self.retired.lock() {
                Ok(mut retired) => {
                    for (call_id, target) in pending.drain() {
                        if target.host_visible {
                            retired.insert(call_id, &target, self.max_pending);
                        }
                    }
                }
                Err(_) => {
                    eprintln!(
                        "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not retire cleared bridge calls"
                    );
                    pending.clear();
                }
            },
            Err(_) => eprintln!(
                "ERR_AGENTOS_BRIDGE_REGISTRY_POISONED: could not clear bridge call registry"
            ),
        }
    }

    #[cfg(test)]
    pub fn pending_len(&self) -> usize {
        self.pending
            .lock()
            .map(|pending| pending.len())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn retired_len(&self) -> usize {
        self.retired
            .lock()
            .map(|retired| retired.by_call_id.len())
            .unwrap_or(0)
    }
}

/// Compatibility name retained while callers migrate from the old
/// call_id-to-session map to direct call-specific settlement.
pub type CallIdRouter = Arc<BridgeCallRegistry>;

/// Shared call_id counter type. Sessions sharing a CallIdRouter must use the same
/// counter to prevent call_id collisions that cause BridgeResponses to be delivered
/// to the wrong session.
pub type SharedCallIdCounter = Arc<AtomicU64>;

/// Cancels a registered route unless ownership has crossed the host-visibility
/// boundary. This closes the prepare-without-dispatch cancellation window for
/// async bridge calls without requiring callers to remember manual cleanup.
struct PendingBridgeRoute {
    registry: CallIdRouter,
    call_id: Option<u64>,
}

impl PendingBridgeRoute {
    fn new(registry: CallIdRouter, call_id: u64) -> Self {
        Self {
            registry,
            call_id: Some(call_id),
        }
    }

    fn disarm(mut self) {
        self.call_id = None;
    }

    fn mark_host_visible(&self) -> Result<(), String> {
        if let Some(call_id) = self.call_id {
            self.registry.mark_host_visible(call_id)
        } else {
            Ok(())
        }
    }

    fn cancel_unpublished(mut self) {
        if let Some(call_id) = self.call_id.take() {
            self.registry.cancel_unpublished(call_id);
        }
    }
}

impl Drop for PendingBridgeRoute {
    fn drop(&mut self) {
        if let Some(call_id) = self.call_id.take() {
            self.registry.cancel(call_id);
        }
    }
}

/// Context for sync-blocking bridge calls from a V8 session.
///
/// Holds the frame sender and response receiver, session ID, call_id counter,
/// and pending-call tracking. Used by V8 FunctionTemplate callbacks to
/// implement the sync-blocking bridge pattern.
pub struct BridgeCallContext {
    /// Sender for serialized frames to the host (channel-based in production)
    sender: Box<dyn RuntimeEventSender>,
    /// Receiver for BridgeResponse frames (no re-serialization needed)
    response_rx: Option<Mutex<Box<dyn BridgeResponseReceiver>>>,
    /// Session ID included in every BridgeCall
    pub session_id: String,
    /// Monotonically increasing call_id counter. Sessions sharing a CallIdRouter
    /// must share the same counter (via Arc) to prevent call_id collisions.
    next_call_id: Arc<AtomicU64>,
    /// Set of in-flight call_ids (for duplicate rejection)
    pending_calls: Mutex<HashSet<u64>>,
    /// Opt-in diagnostic tracking for sync call_ids. The atomic call_id counter
    /// plus recv_response(call_id) validation are the correctness path; this set
    /// is only needed when inspecting in-flight calls.
    track_pending_calls: bool,
    /// Optional direct call-specific response registry.
    call_id_router: Option<CallIdRouter>,
    session_generation: Option<u64>,
    async_response_tx: Option<crossbeam_channel::Sender<BridgeResponse>>,
    abort_rx: Option<crossbeam_channel::Receiver<()>>,
    /// Execution gate shared with the owning session. Direct response routing
    /// bypasses the ordinary command lane, but a sync response must still stop
    /// at this boundary while the VM is paused.
    pause_control: Option<Arc<crate::session::SessionPauseControl>>,
    /// Session-injected process scheduler used by local bridge operations such
    /// as `node:vm` timeouts. Snapshot/test-only contexts may omit it when they
    /// never arm runtime work.
    runtime: Option<agentos_runtime::RuntimeContext>,
    bridge_call_timeout: Duration,
}

pub struct PreparedAsyncBridgeCall {
    pub(crate) call_id: u64,
    event: RuntimeEvent,
    pending_route: Option<PendingBridgeRoute>,
}

/// No-op FrameSender for snapshot stub functions.
/// Panics if called — stubs must never be invoked during snapshot creation.
#[allow(dead_code)]
struct StubRuntimeEventSender;

impl RuntimeEventSender for StubRuntimeEventSender {
    fn send_event(&self, _event: RuntimeEvent) -> Result<(), String> {
        panic!(
            "stub bridge function called during snapshot creation — bridge IIFE must not call bridge functions at setup time"
        )
    }
}

/// No-op ResponseReceiver for snapshot stub functions.
/// Panics if called — stubs must never be invoked during snapshot creation.
#[allow(dead_code)]
struct StubBridgeResponseReceiver;

impl BridgeResponseReceiver for StubBridgeResponseReceiver {
    fn recv_response(&self, _expected_call_id: u64) -> Result<BridgeResponse, String> {
        panic!(
            "stub bridge function called during snapshot creation — bridge IIFE must not call bridge functions at setup time"
        )
    }
}

#[allow(dead_code)]
impl BridgeCallContext {
    /// Create a no-op BridgeCallContext for snapshot stub functions.
    /// Panics if sync_call or async_send is called — stubs exist only for
    /// the bridge IIFE to reference (not call) during snapshot creation.
    pub fn stub() -> Self {
        BridgeCallContext {
            sender: Box::new(StubRuntimeEventSender),
            response_rx: Some(Mutex::new(Box::new(StubBridgeResponseReceiver))),
            session_id: "stub".into(),
            next_call_id: Arc::new(AtomicU64::new(1)),
            pending_calls: Mutex::new(HashSet::new()),
            track_pending_calls: should_track_pending_sync_calls(),
            call_id_router: None,
            session_generation: None,
            async_response_tx: None,
            abort_rx: None,
            pause_control: None,
            runtime: None,
            bridge_call_timeout: DEFAULT_BRIDGE_CALL_TIMEOUT,
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
            sender: Box::new(WriterRuntimeEventSender {
                writer: Mutex::new(writer),
            }),
            response_rx: Some(Mutex::new(Box::new(ReaderBridgeResponseReceiver::new(
                reader,
            )))),
            session_id,
            next_call_id: Arc::new(AtomicU64::new(1)),
            pending_calls: Mutex::new(HashSet::new()),
            track_pending_calls: should_track_pending_sync_calls(),
            call_id_router: None,
            session_generation: None,
            async_response_tx: None,
            abort_rx: None,
            pause_control: None,
            runtime: None,
            bridge_call_timeout: DEFAULT_BRIDGE_CALL_TIMEOUT,
        }
    }

    /// Create a BridgeCallContext with a FrameSender, ResponseReceiver, call_id routing table,
    /// and shared call_id counter. All sessions sharing the same CallIdRouter must share
    /// the same counter to prevent call_id collisions in the routing table.
    pub fn with_receiver(
        sender: Box<dyn RuntimeEventSender>,
        response_rx: Box<dyn BridgeResponseReceiver>,
        session_id: String,
        _router: CallIdRouter,
        shared_call_id: SharedCallIdCounter,
    ) -> Self {
        BridgeCallContext {
            sender,
            response_rx: Some(Mutex::new(response_rx)),
            session_id,
            next_call_id: shared_call_id,
            pending_calls: Mutex::new(HashSet::new()),
            track_pending_calls: should_track_pending_sync_calls(),
            call_id_router: None,
            session_generation: None,
            async_response_tx: None,
            abort_rx: None,
            pause_control: None,
            runtime: None,
            bridge_call_timeout: DEFAULT_BRIDGE_CALL_TIMEOUT,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with_registry(
        sender: Box<dyn RuntimeEventSender>,
        session_id: String,
        session_generation: Option<u64>,
        registry: CallIdRouter,
        shared_call_id: SharedCallIdCounter,
        async_response_tx: crossbeam_channel::Sender<BridgeResponse>,
        abort_rx: crossbeam_channel::Receiver<()>,
        runtime: agentos_runtime::RuntimeContext,
        pause_control: Arc<crate::session::SessionPauseControl>,
        bridge_call_timeout: Duration,
    ) -> Self {
        BridgeCallContext {
            sender,
            response_rx: None,
            session_id,
            next_call_id: shared_call_id,
            pending_calls: Mutex::new(HashSet::new()),
            track_pending_calls: should_track_pending_sync_calls(),
            call_id_router: Some(registry),
            session_generation,
            async_response_tx: Some(async_response_tx),
            abort_rx: Some(abort_rx),
            pause_control: Some(pause_control),
            runtime: Some(runtime),
            bridge_call_timeout,
        }
    }

    pub(crate) fn runtime_context(&self) -> Option<&agentos_runtime::RuntimeContext> {
        self.runtime.as_ref()
    }

    pub(crate) fn timer_task_owner(&self) -> Option<agentos_runtime::TaskOwner> {
        self.session_generation
            .map(|generation| agentos_runtime::TaskOwner::Vm { generation })
    }

    /// Perform a sync-blocking bridge call.
    ///
    /// Generates a unique call_id, sends a BridgeCall message over IPC,
    /// blocks on read() for the BridgeResponse, and returns the result.
    /// Error responses from the host are returned as Err.
    pub fn sync_call_response(
        &self,
        method: &str,
        args: Vec<u8>,
    ) -> Result<Option<SyncBridgeCallResponse>, String> {
        let max_response_bytes = self.configured_bridge_response_bytes(method)?;
        self.sync_call_response_with_max_response_bytes(method, args, max_response_bytes)
    }

    pub fn sync_call_response_with_max_response_bytes(
        &self,
        method: &str,
        args: Vec<u8>,
        max_response_bytes: usize,
    ) -> Result<Option<SyncBridgeCallResponse>, String> {
        let response =
            self.sync_call_frame_with_max_response_bytes(method, args, max_response_bytes)?;
        match response {
            Some(response) if response.status == 1 => {
                Err(bridge_error_payload_message(&response.payload))
            }
            response => Ok(response),
        }
    }

    /// Perform a sync bridge call while preserving the structured status=1
    /// payload. The V8 adapter uses this to attach typed error fields without
    /// reconstructing an errno from a diagnostic string.
    pub fn sync_call_frame_with_max_response_bytes(
        &self,
        method: &str,
        args: Vec<u8>,
        max_response_bytes: usize,
    ) -> Result<Option<SyncBridgeCallResponse>, String> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);
        track_sync_bridge_call_method(call_id, method);

        // Optional diagnostic tracking. Correctness comes from the atomic
        // counter and recv_response(call_id) identity validation.
        if self.track_pending_calls {
            let mut pending = match self.pending_calls.lock() {
                Ok(pending) => pending,
                Err(_) => {
                    cleanup_sync_bridge_call_tracking(call_id);
                    return Err(String::from(
                        "ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED: pending bridge-call lock poisoned during admission",
                    ));
                }
            };
            if !pending.insert(call_id) {
                drop(pending);
                cleanup_sync_bridge_call_tracking(call_id);
                return Err(format!("duplicate call_id: {}", call_id));
            }
        }

        let (direct_response_rx, mut pending_route) = if let Some(ref registry) =
            self.call_id_router
        {
            let phase_start = Instant::now();
            let runtime = match self.runtime.as_ref() {
                Some(runtime) => runtime,
                None => {
                    return Err(self.cleanup_pending_call_after_error(
                        call_id,
                        String::from(
                    "ERR_AGENTOS_RUNTIME_NOT_INJECTED: direct bridge calls require a session RuntimeContext",
                        ),
                    ));
                }
            };
            let receiver = match registry.register_sync_with_timeout(
                runtime,
                args.len(),
                max_response_bytes,
                call_id,
                &self.session_id,
                self.session_generation,
                self.bridge_call_timeout,
            ) {
                Ok(receiver) => receiver,
                Err(error) => {
                    return Err(self.cleanup_pending_call_after_error(call_id, error));
                }
            };
            record_sync_bridge_host_phase(method, "host_register_route", phase_start.elapsed());
            (
                Some(receiver),
                Some(PendingBridgeRoute::new(Arc::clone(registry), call_id)),
            )
        } else {
            (None, None)
        };

        // Send BridgeCall to host
        let bridge_call = RuntimeEvent::BridgeCall {
            session_id: self.session_id.clone(),
            call_id,
            method: method.to_string(),
            payload: args,
        };

        let __lat = syncrpc_lat_enabled().then(Instant::now);
        let phase_start = Instant::now();
        if let Some(route) = pending_route.as_ref() {
            if let Err(error) = route.mark_host_visible() {
                return Err(self.cleanup_pending_call_after_error(call_id, error));
            }
        }
        if let Err(e) = self.sender.send_event(bridge_call) {
            if let Some(route) = pending_route.take() {
                route.cancel_unpublished();
            }
            return Err(self.cleanup_pending_call_after_error(
                call_id,
                format!("failed to write BridgeCall: {}", e),
            ));
        }
        record_sync_bridge_host_phase(method, "host_send_event", phase_start.elapsed());

        // Receive BridgeResponse directly (no re-serialization)
        let response = if let Some(receiver) = direct_response_rx {
            let phase_start = Instant::now();
            let result = if let Some(abort_rx) = self.abort_rx.as_ref() {
                crossbeam_channel::select! {
                    recv(receiver) -> response => response.map_err(|_| {
                        String::from("bridge response target closed before settlement")
                    }),
                    recv(abort_rx) -> _ => Err(String::from("execution aborted")),
                    default(self.bridge_call_timeout) => {
                        let registry = self.call_id_router.as_ref().expect("direct response route");
                        match registry.timeout(call_id) {
                            Ok(_) => receiver.recv().map_err(|_| {
                                String::from("bridge response target closed during deadline settlement")
                            }),
                            Err(error) => Err(error),
                        }
                    },
                }
            } else {
                match receiver.recv_timeout(self.bridge_call_timeout) {
                    Ok(response) => Ok(response),
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(String::from(
                        "bridge response target closed before settlement",
                    )),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        let registry = self.call_id_router.as_ref().expect("direct response route");
                        match registry.timeout(call_id) {
                            Ok(_) => receiver.recv().map_err(|_| {
                                String::from(
                                    "bridge response target closed during deadline settlement",
                                )
                            }),
                            Err(error) => Err(error),
                        }
                    }
                }
            };
            match result {
                Ok(frame) => {
                    if let Some(control) = &self.pause_control {
                        control.wait_while_paused();
                    }
                    record_sync_bridge_host_phase(
                        method,
                        "host_recv_response",
                        phase_start.elapsed(),
                    );
                    frame
                }
                Err(e) => {
                    return Err(self.cleanup_pending_call_after_error(
                        call_id,
                        format!("{e}; bridge_method={method}"),
                    ));
                }
            }
        } else {
            let rx = self
                .response_rx
                .as_ref()
                .expect("legacy bridge context has a response receiver")
                .lock()
                .unwrap();
            let phase_start = Instant::now();
            match rx.recv_response(call_id) {
                Ok(frame) => {
                    record_sync_bridge_host_phase(
                        method,
                        "host_recv_response",
                        phase_start.elapsed(),
                    );
                    frame
                }
                Err(e) => {
                    return Err(self.cleanup_pending_call_after_error(call_id, e));
                }
            }
        };
        if let Some(t) = __lat {
            record_syncrpc_lat(t.elapsed().as_nanos() as u64);
        }

        let phase_start = Instant::now();
        self.remove_pending_call(call_id)?;
        record_sync_bridge_host_phase(method, "host_cleanup", phase_start.elapsed());

        // Validate and extract BridgeResponse
        let phase_start = Instant::now();
        if response.payload.is_empty() && response.status == 0 {
            record_sync_bridge_host_phase(method, "host_extract_response", phase_start.elapsed());
            Ok(None)
        } else {
            let result = Ok(Some(SyncBridgeCallResponse {
                status: response.status,
                payload: response.payload,
                reservation: response.reservation,
            }));
            record_sync_bridge_host_phase(method, "host_extract_response", phase_start.elapsed());
            result
        }
    }

    pub fn sync_call(&self, method: &str, args: Vec<u8>) -> Result<Option<Vec<u8>>, String> {
        self.sync_call_response(method, args)
            .map(|response| response.map(|response| response.payload))
    }

    pub fn prepare_async_call(
        &self,
        method: &str,
        args: Vec<u8>,
    ) -> Result<PreparedAsyncBridgeCall, String> {
        let max_response_bytes = self.configured_bridge_response_bytes(method)?;
        self.prepare_async_call_with_max_response_bytes(method, args, max_response_bytes)
    }

    pub fn prepare_async_call_with_max_response_bytes(
        &self,
        method: &str,
        args: Vec<u8>,
        max_response_bytes: usize,
    ) -> Result<PreparedAsyncBridgeCall, String> {
        let call_id = self.next_call_id.fetch_add(1, Ordering::Relaxed);

        let pending_route = if let Some(ref registry) = self.call_id_router {
            let runtime = self.runtime.as_ref().ok_or_else(|| {
                String::from(
                    "ERR_AGENTOS_RUNTIME_NOT_INJECTED: direct bridge calls require a session RuntimeContext",
                )
            })?;
            let sender = self.async_response_tx.as_ref().ok_or_else(|| {
                String::from(
                    "ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY: async response lane is unavailable",
                )
            })?;
            if bridge_call_uses_session_lifetime(method) {
                registry.register_async_for_session_lifetime(
                    runtime,
                    args.len(),
                    max_response_bytes,
                    call_id,
                    &self.session_id,
                    self.session_generation,
                    sender.clone(),
                )?;
            } else {
                let mut deadline_cancelled = registry.register_async_with_timeout(
                    runtime,
                    args.len(),
                    max_response_bytes,
                    call_id,
                    &self.session_id,
                    self.session_generation,
                    sender.clone(),
                    self.bridge_call_timeout,
                )?;
                let deadline_registry = Arc::clone(registry);
                let timeout = self.bridge_call_timeout;
                if let Err(error) = runtime.spawn(agentos_runtime::TaskClass::Timer, async move {
                    if tokio::time::timeout(timeout, &mut deadline_cancelled)
                        .await
                        .is_err()
                    {
                        if let Err(error) = deadline_registry.timeout(call_id) {
                            eprintln!("{error}");
                        }
                    }
                }) {
                    // Registration already owns call/request/response capacity.
                    // If supervision rejects the timer, retract the unpublished
                    // route before returning so admission cannot leak permanently.
                    registry.cancel_unpublished(call_id);
                    return Err(format!(
                        "ERR_AGENTOS_BRIDGE_DEADLINE_TASK: failed to arm bridge call_id {call_id} deadline: {error}"
                    ));
                }
            }
            Some(PendingBridgeRoute::new(Arc::clone(registry), call_id))
        } else {
            None
        };

        Ok(PreparedAsyncBridgeCall {
            call_id,
            event: RuntimeEvent::BridgeCall {
                session_id: self.session_id.clone(),
                call_id,
                method: method.to_string(),
                payload: args,
            },
            pending_route,
        })
    }

    fn configured_bridge_response_bytes(&self, method: &str) -> Result<usize, String> {
        if self.call_id_router.is_none() {
            return Ok(0);
        }
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            String::from(
                "ERR_AGENTOS_RUNTIME_NOT_INJECTED: direct bridge calls require a session RuntimeContext",
            )
        })?;
        let configured = runtime
            .resources()
            .usage(ResourceClass::BridgeResponseBytes)
            .limit
            .ok_or_else(|| {
                String::from(
                    "ERR_AGENTOS_RESOURCE_UNBOUNDED: bridge response bytes require limits.reactor.maxBridgeResponseBytes",
                )
            })?;
        Ok(configured.min(crate::bridge::declared_bridge_response_bytes(method, None)))
    }

    pub fn dispatch_async_call(&self, prepared: PreparedAsyncBridgeCall) -> Result<u64, String> {
        let PreparedAsyncBridgeCall {
            call_id,
            event,
            mut pending_route,
        } = prepared;
        if let Some(route) = pending_route.as_ref() {
            route.mark_host_visible()?;
        }
        if let Err(e) = self.sender.send_event(event) {
            if let Some(route) = pending_route.take() {
                route.cancel_unpublished();
            }
            return Err(format!("failed to write BridgeCall: {}", e));
        }
        if let Some(pending_route) = pending_route {
            pending_route.disarm();
        }
        Ok(call_id)
    }

    /// Legacy one-step helper used by non-concurrent tests.
    pub fn async_send(&self, method: &str, args: Vec<u8>) -> Result<u64, String> {
        let prepared = self.prepare_async_call(method, args)?;
        self.dispatch_async_call(prepared)
    }

    fn remove_pending_call(&self, call_id: u64) -> Result<(), String> {
        cleanup_sync_bridge_call_tracking(call_id);
        if self.track_pending_calls {
            self.pending_calls
                .lock()
                .map_err(|_| {
                    String::from(
                        "ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED: pending bridge-call lock poisoned during cleanup",
                    )
                })?
                .remove(&call_id);
        }
        Ok(())
    }

    fn cleanup_pending_call_after_error(&self, call_id: u64, error: String) -> String {
        match self.remove_pending_call(call_id) {
            Ok(()) => error,
            Err(cleanup_error) => format!("{cleanup_error}; original_error={error}"),
        }
    }

    /// Check if a call_id is currently pending.
    pub fn is_call_pending(&self, call_id: u64) -> Result<bool, String> {
        if !self.track_pending_calls {
            return Ok(false);
        }
        self.pending_calls
            .lock()
            .map(|pending| pending.contains(&call_id))
            .map_err(|_| {
                String::from(
                    "ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED: pending bridge-call lock poisoned during inspection",
                )
            })
    }

    /// Number of pending calls.
    pub fn pending_count(&self) -> Result<usize, String> {
        if !self.track_pending_calls {
            return Ok(0);
        }
        self.pending_calls
            .lock()
            .map(|pending| pending.len())
            .map_err(|_| {
                String::from(
                    "ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED: pending bridge-call lock poisoned during inspection",
                )
            })
    }
}

impl Drop for BridgeCallContext {
    fn drop(&mut self) {
        if let Some(registry) = self.call_id_router.as_ref() {
            registry.cancel_session(&self.session_id, self.session_generation);
        }
    }
}

fn should_track_pending_sync_calls() -> bool {
    std::env::var("AGENTOS_TRACK_PENDING_SYNC_CALLS").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    fn test_runtime_context() -> RuntimeContext {
        crate::test_runtime_context()
    }

    fn limited_bridge_runtime(
        max_calls: usize,
        max_request_bytes: usize,
        max_response_bytes: usize,
    ) -> (
        RuntimeContext,
        Arc<agentos_runtime::accounting::ResourceLedger>,
    ) {
        let process = test_runtime_context();
        let resources = Arc::new(agentos_runtime::accounting::ResourceLedger::child(
            "bridge-test-vm",
            [
                (
                    ResourceClass::BridgeCalls,
                    agentos_runtime::accounting::ResourceLimit::new(
                        max_calls,
                        "limits.reactor.maxBridgeCalls",
                    ),
                ),
                (
                    ResourceClass::BridgeRequestBytes,
                    agentos_runtime::accounting::ResourceLimit::new(
                        max_request_bytes,
                        "limits.reactor.maxBridgeRequestBytes",
                    ),
                ),
                (
                    ResourceClass::BridgeResponseBytes,
                    agentos_runtime::accounting::ResourceLimit::new(
                        max_response_bytes,
                        "limits.reactor.maxBridgeResponseBytes",
                    ),
                ),
            ],
            Arc::clone(process.resources()),
        ));
        (process.scoped_for_vm(Arc::clone(&resources), 7), resources)
    }

    fn assert_ledger_settles_to_zero(resources: &agentos_runtime::accounting::ResourceLedger) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while !resources.is_zero() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            resources.is_zero(),
            "bridge reservations and supervised deadline task must reconcile"
        );
    }

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

    struct RejectingRuntimeEventSender;

    impl RuntimeEventSender for RejectingRuntimeEventSender {
        fn send_event(&self, _event: RuntimeEvent) -> Result<(), String> {
            Err(String::from("injected event-lane failure"))
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
        let payload = encode_terminal_error_payload("ENOENT", "no such file", 4096);
        let mut response_bytes = Vec::new();
        ipc_binary::write_frame(
            &mut response_bytes,
            &BinaryFrame::BridgeResponse {
                session_id: String::new(),
                call_id: 1,
                status: 1,
                payload,
            },
        )
        .expect("encode structured error response");
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
    fn terminal_error_payload_never_derives_code_from_message() {
        let payload =
            encode_terminal_error_payload("EIO", "EACCES: guest-controlled diagnostic", 4096);
        let ciborium::Value::Map(entries) =
            ciborium::from_reader::<ciborium::Value, _>(payload.as_slice())
                .expect("decode structured bridge error")
        else {
            panic!("structured bridge error must be a CBOR map");
        };
        let text_field = |name: &str| {
            entries.iter().find_map(|(key, value)| match (key, value) {
                (ciborium::Value::Text(key), ciborium::Value::Text(value)) if key == name => {
                    Some(value.as_str())
                }
                _ => None,
            })
        };
        assert_eq!(text_field("code"), Some("EIO"));
        assert_eq!(
            text_field("message"),
            Some("EACCES: guest-controlled diagnostic")
        );
    }

    #[test]
    fn sync_call_frame_preserves_structured_error_payload() {
        let payload = encode_terminal_error_payload("EACCES", "permission denied", 4096);
        let mut response_bytes = Vec::new();
        ipc_binary::write_frame(
            &mut response_bytes,
            &BinaryFrame::BridgeResponse {
                session_id: String::new(),
                call_id: 1,
                status: 1,
                payload: payload.clone(),
            },
        )
        .expect("encode structured error response");
        let ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(response_bytes)),
            "session-1".into(),
        );

        let response = ctx
            .sync_call_frame_with_max_response_bytes("_fsReadFile", vec![0xc0], 4096)
            .expect("receive structured response")
            .expect("error response frame");
        assert_eq!(response.status, 1);
        assert_eq!(response.payload, payload);
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

        assert_eq!(ctx.pending_count().expect("inspect pending calls"), 0);
        let _ = ctx.sync_call("_fn", vec![]);
        assert_eq!(ctx.pending_count().expect("inspect pending calls"), 0);
    }

    #[test]
    fn poisoned_pending_call_state_returns_a_typed_error() {
        let mut ctx = BridgeCallContext::new(
            Box::new(Vec::new()),
            Box::new(Cursor::new(Vec::new())),
            "session-1".into(),
        );
        ctx.track_pending_calls = true;

        std::thread::scope(|scope| {
            let pending_calls = &ctx.pending_calls;
            assert!(
                scope
                    .spawn(|| {
                        let _guard = pending_calls.lock().expect("acquire pending-call lock");
                        panic!("poison pending-call lock");
                    })
                    .join()
                    .is_err(),
                "test poisoner must panic"
            );
        });

        let error = ctx
            .sync_call("_fn", vec![])
            .expect_err("poisoned correctness state must reject admission");
        assert!(error.starts_with("ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED:"));
        assert!(ctx
            .is_call_pending(1)
            .expect_err("poisoned inspection must be typed")
            .starts_with("ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED:"));
        assert!(ctx
            .pending_count()
            .expect_err("poisoned inspection must be typed")
            .starts_with("ERR_AGENTOS_BRIDGE_PENDING_CALLS_POISONED:"));
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
    fn channel_runtime_event_sender_delivers_frames() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = super::ChannelRuntimeEventSender::new(tx, None);

        let event = RuntimeEvent::BridgeCall {
            session_id: "sess-1".into(),
            call_id: 42,
            method: "_fsReadFile".into(),
            payload: vec![0x01, 0x02],
        };
        sender.send_event(event.clone()).expect("send_event");

        // Verify the received event matches without any BinaryFrame hop.
        let received = rx.recv().expect("recv");
        assert_eq!(received.output_generation, None);
        assert_eq!(received.event, event);
    }

    #[test]
    fn channel_runtime_event_sender_no_mutex_contention() {
        // Multiple senders can send concurrently without blocking each other
        let (tx, rx) = crossbeam_channel::unbounded();
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let sender = super::ChannelRuntimeEventSender::new(tx.clone(), None);
                std::thread::spawn(move || {
                    for j in 0..10 {
                        let event = RuntimeEvent::BridgeCall {
                            session_id: format!("sess-{}", i),
                            call_id: (i * 100 + j) as u64,
                            method: "_fn".into(),
                            payload: vec![],
                        };
                        sender.send_event(event).expect("send_event");
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
        while rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 40);
    }

    #[test]
    fn channel_runtime_event_sender_with_bridge_context() {
        // Verify BridgeCallContext works with ChannelRuntimeEventSender end-to-end
        let (tx, rx) = crossbeam_channel::unbounded();

        // Pre-serialize a BridgeResponse for the reader
        let response_bytes = make_response_bytes(1, Some(vec![0xAB, 0xCD]), None);
        let router: super::CallIdRouter = Arc::new(super::BridgeCallRegistry::with_default_limit());

        let ctx = BridgeCallContext::with_receiver(
            Box::new(super::ChannelRuntimeEventSender::new(tx, None)),
            Box::new(super::ReaderBridgeResponseReceiver::new(Box::new(
                Cursor::new(response_bytes),
            ))),
            "test-session".into(),
            router,
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        );

        let result = ctx.sync_call("_fsReadFile", vec![0x01]).unwrap();
        assert_eq!(result, Some(vec![0xAB, 0xCD]));

        // Verify the BridgeCall went through the channel
        let event = rx.recv().expect("recv bridge call");
        match event.event {
            RuntimeEvent::BridgeCall { method, .. } => assert_eq!(method, "_fsReadFile"),
            _ => panic!("expected BridgeCall"),
        }
    }

    #[test]
    fn sync_call_success_clears_call_id_route() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let response_bytes = make_response_bytes(1, Some(vec![0xAB, 0xCD]), None);
        let router: super::CallIdRouter = Arc::new(super::BridgeCallRegistry::with_default_limit());

        let ctx = BridgeCallContext::with_receiver(
            Box::new(super::ChannelRuntimeEventSender::new(tx, None)),
            Box::new(super::ReaderBridgeResponseReceiver::new(Box::new(
                Cursor::new(response_bytes),
            ))),
            "test-session".into(),
            Arc::clone(&router),
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        );

        let result = ctx.sync_call("_fsReadFile", vec![0x01]).unwrap();
        assert_eq!(result, Some(vec![0xAB, 0xCD]));
        assert!(
            router.pending_len() == 0,
            "sync bridge response completion should clear the call_id route"
        );
    }

    #[test]
    fn bridge_registry_settles_a_call_specific_waiter_directly() {
        let registry = BridgeCallRegistry::new(2);
        let runtime = test_runtime_context();
        let waiter = registry
            .register_sync(&runtime, 0, 1, 7, "session-a", Some(3))
            .expect("register direct waiter");

        registry
            .settle(
                "session-a",
                Some(3),
                BridgeResponse {
                    call_id: 7,
                    status: 0,
                    payload: vec![0xA7],
                    reservation: None,
                },
            )
            .expect("settle direct waiter");

        assert_eq!(waiter.recv().expect("direct response").payload, vec![0xA7]);
        assert_eq!(registry.pending_len(), 0);
    }

    #[test]
    fn bridge_registry_rejects_stale_generation_without_consuming_target() {
        let registry = BridgeCallRegistry::new(1);
        let runtime = test_runtime_context();
        let waiter = registry
            .register_sync(&runtime, 0, 1, 8, "session-a", Some(4))
            .expect("register direct waiter");
        let response = BridgeResponse {
            call_id: 8,
            status: 0,
            payload: vec![0xA8],
            reservation: None,
        };

        let error = registry
            .settle("session-a", Some(5), response.clone())
            .expect_err("stale response must be rejected");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_STALE_GENERATION"));
        assert_eq!(registry.pending_len(), 1);

        registry
            .settle("session-a", Some(4), response)
            .expect("correct generation should still settle");
        assert_eq!(waiter.recv().expect("direct response").call_id, 8);
    }

    #[test]
    fn bridge_registry_only_classifies_canceled_host_visible_routes_as_stale() {
        let registry = BridgeCallRegistry::new(2);
        let runtime = test_runtime_context();

        let _visible_waiter = registry
            .register_sync(&runtime, 0, 1, 20, "session-a", Some(4))
            .expect("register host-visible route");
        registry
            .mark_host_visible(20)
            .expect("mark route host-visible");
        registry.cancel_session("session-a", Some(4));
        let error = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 20,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("canceled host-visible route must reject a stale completion");
        assert_eq!(error.kind(), BridgeSettlementErrorKind::StaleCompletion);
        assert!(error.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));

        let mismatched = registry
            .settle(
                "session-a",
                Some(5),
                BridgeResponse {
                    call_id: 20,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("a different generation must not inherit stale-completion status");
        assert_eq!(mismatched.kind(), BridgeSettlementErrorKind::Other);
        assert!(mismatched.contains("ERR_AGENTOS_BRIDGE_STALE_GENERATION"));

        let _unpublished_waiter = registry
            .register_sync(&runtime, 0, 1, 21, "session-a", Some(4))
            .expect("register unpublished route");
        registry.cancel(21);
        let unknown = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 21,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("unpublished route must not authorize a stale response");
        assert!(unknown.contains("ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID"));
        assert_eq!(registry.retired_len(), 1);
    }

    #[test]
    fn bridge_registry_classifies_only_published_disconnected_waiters_as_stale() {
        let registry = BridgeCallRegistry::new(2);
        let runtime = test_runtime_context();

        let visible_waiter = registry
            .register_sync(&runtime, 0, 1, 23, "session-a", Some(4))
            .expect("register host-visible route");
        registry
            .mark_host_visible(23)
            .expect("mark route host-visible");
        drop(visible_waiter);

        let stale = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 23,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("a published route may disconnect during guest teardown");
        assert_eq!(stale.kind(), BridgeSettlementErrorKind::StaleCompletion);
        assert!(stale.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.retired_len(), 1);

        let unpublished_waiter = registry
            .register_sync(&runtime, 0, 1, 24, "session-a", Some(4))
            .expect("register unpublished route");
        drop(unpublished_waiter);

        let delivery_error = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 24,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("an unpublished disconnected route remains a hard error");
        assert_eq!(delivery_error.kind(), BridgeSettlementErrorKind::Other);
        assert!(delivery_error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY"));
        assert_eq!(registry.retired_len(), 1);
    }

    #[test]
    fn bridge_registry_does_not_hide_duplicate_settlement_as_teardown() {
        let registry = BridgeCallRegistry::new(1);
        let runtime = test_runtime_context();
        let waiter = registry
            .register_sync(&runtime, 0, 1, 22, "session-a", Some(4))
            .expect("register direct waiter");
        registry
            .mark_host_visible(22)
            .expect("mark route host-visible");
        let response = BridgeResponse {
            call_id: 22,
            status: 0,
            payload: vec![0xA2],
            reservation: None,
        };
        registry
            .settle("session-a", Some(4), response.clone())
            .expect("settle direct waiter");
        drop(waiter.recv().expect("receive direct response"));

        let duplicate = registry
            .settle("session-a", Some(4), response)
            .expect_err("duplicate settlement must remain a hard error");
        assert!(duplicate.contains("ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID"));
        assert_eq!(registry.retired_len(), 0);
    }

    #[test]
    fn bridge_registry_bounds_retired_identity_history_and_releases_reservations() {
        let registry = BridgeCallRegistry::new(1);
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);

        for call_id in [30, 31] {
            let _waiter = registry
                .register_sync(&runtime, 2, 4, call_id, "session-a", Some(4))
                .expect("register route for cancellation");
            registry
                .mark_host_visible(call_id)
                .expect("mark route host-visible");
            registry.cancel_session("session-a", Some(4));
            assert!(
                resources.is_zero(),
                "cancellation must release reservations"
            );
            assert_eq!(registry.retired_len(), 1);
        }

        let evicted = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 30,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("oldest retired identity must be evicted at the configured bound");
        assert!(evicted.contains("ERR_AGENTOS_BRIDGE_UNKNOWN_CALL_ID"));

        let retained = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 31,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("newest retired identity must remain classified");
        assert!(retained.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));
    }

    #[test]
    fn bridge_registry_clear_releases_all_pre_reserved_response_capacity() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 4, 8);

        for (call_id, session_id) in [(32, "session-a"), (33, "session-b")] {
            let _waiter = registry
                .register_sync(&runtime, 1, 4, call_id, session_id, Some(4))
                .expect("pre-reserve response capacity before teardown");
            registry
                .mark_host_visible(call_id)
                .expect("mark route host-visible");
        }
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 2);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 8);

        registry.clear();

        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.retired_len(), 2);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_stale_completion_cannot_consume_reused_session_generation() {
        let registry = BridgeCallRegistry::new(2);
        let runtime = test_runtime_context();
        let _old_waiter = registry
            .register_sync(&runtime, 0, 1, 40, "reused-session", Some(4))
            .expect("register old generation route");
        registry
            .mark_host_visible(40)
            .expect("mark old route host-visible");
        registry.cancel_session("reused-session", Some(4));

        let current_waiter = registry
            .register_sync(&runtime, 0, 1, 41, "reused-session", Some(5))
            .expect("register current generation route");
        registry
            .mark_host_visible(41)
            .expect("mark current route host-visible");

        let stale = registry
            .settle(
                "reused-session",
                Some(4),
                BridgeResponse {
                    call_id: 40,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("retired generation must reject its late response");
        assert!(stale.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));
        assert_eq!(registry.pending_len(), 1);

        registry
            .settle(
                "reused-session",
                Some(5),
                BridgeResponse {
                    call_id: 41,
                    status: 0,
                    payload: vec![0xA5],
                    reservation: None,
                },
            )
            .expect("settle current generation");
        assert_eq!(
            current_waiter
                .recv()
                .expect("current generation response")
                .payload,
            vec![0xA5]
        );
    }

    #[test]
    fn sync_call_teardown_after_publication_records_stale_completion_proof() {
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(2));
        let runtime = test_runtime_context();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(4))),
            String::from("session-a"),
            Some(4),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(50)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        let guest = std::thread::spawn(move || {
            ctx.sync_call_response_with_max_response_bytes("_fsReadFile", Vec::new(), 1)
        });
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("host must observe bridge call before teardown");
        assert!(matches!(
            event.event,
            RuntimeEvent::BridgeCall { call_id: 50, .. }
        ));

        registry.cancel_session("session-a", Some(4));
        let _ = abort_tx.try_send(());
        let guest_error = guest
            .join()
            .expect("join guest bridge call")
            .expect_err("teardown must abort sync call");
        assert!(
            guest_error.contains("bridge response target closed")
                || guest_error.contains("execution aborted")
        );

        let stale = registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 50,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("late host response must use retirement proof");
        assert!(stale.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));
    }

    #[test]
    fn sync_bridge_call_deadline_releases_route_and_rejects_late_response() {
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(2));
        let runtime = test_runtime_context();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(4))),
            String::from("session-deadline"),
            Some(4),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(60)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            Duration::from_millis(10),
        );

        let error = ctx
            .sync_call_response_with_max_response_bytes("_fsReadFile", Vec::new(), 128)
            .expect_err("missing host response must hit the per-call deadline");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_CALL_TIMEOUT"));
        assert_eq!(registry.pending_len(), 0);
        assert_eq!(registry.retired_len(), 1);
        assert!(matches!(
            event_rx.recv().expect("host-visible call").event,
            RuntimeEvent::BridgeCall { call_id: 60, .. }
        ));
        let late = registry
            .settle(
                "session-deadline",
                Some(4),
                BridgeResponse {
                    call_id: 60,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect_err("late host response must not resurrect the timed-out route");
        assert!(late.contains("ERR_AGENTOS_BRIDGE_STALE_COMPLETION"));
    }

    #[test]
    fn async_bridge_call_deadline_settles_dedicated_response_lane() {
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(2));
        let runtime = test_runtime_context();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(5))),
            String::from("async-deadline"),
            Some(5),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(70)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            Duration::from_millis(10),
        );

        assert_eq!(ctx.async_send("_toolCall", Vec::new()).unwrap(), 70);
        assert!(matches!(
            event_rx.recv().expect("host-visible async call").event,
            RuntimeEvent::BridgeCall { call_id: 70, .. }
        ));
        let timeout = async_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("deadline must settle the async lane");
        assert_eq!(timeout.status, 1);
        assert!(
            String::from_utf8_lossy(&timeout.payload).contains("ERR_AGENTOS_BRIDGE_CALL_TIMEOUT")
        );
        assert_eq!(registry.pending_len(), 0);
    }

    #[test]
    fn direct_sync_response_waits_at_paused_execution_boundary() {
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(2));
        let runtime = test_runtime_context();
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let pause_control = Arc::new(crate::session::SessionPauseControl::default());
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(4))),
            String::from("session-a"),
            Some(4),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(50)),
            async_tx,
            abort_rx,
            runtime,
            Arc::clone(&pause_control),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        pause_control.pause();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let guest = std::thread::spawn(move || {
            let _ = result_tx.send(ctx.sync_call_response_with_max_response_bytes(
                "_fsReadFile",
                Vec::new(),
                1,
            ));
        });
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("host must observe the direct bridge call");
        assert!(matches!(
            event.event,
            RuntimeEvent::BridgeCall { call_id: 50, .. }
        ));

        registry
            .settle(
                "session-a",
                Some(4),
                BridgeResponse {
                    call_id: 50,
                    status: 0,
                    payload: Vec::new(),
                    reservation: None,
                },
            )
            .expect("settle direct response while paused");
        assert!(
            matches!(
                result_rx.recv_timeout(Duration::from_millis(50)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "direct response must not let synchronous JavaScript cross a paused boundary"
        );

        pause_control.resume();
        result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("resume must release the direct response")
            .expect("direct sync bridge call must complete successfully");
        guest.join().expect("join direct bridge caller");
    }

    #[test]
    fn bridge_registry_enforces_its_configured_bound() {
        let registry = BridgeCallRegistry::new(1);
        let runtime = test_runtime_context();
        let _waiter = registry
            .register_sync(&runtime, 0, 1, 9, "session-a", None)
            .expect("register first waiter");

        let error = registry
            .register_sync(&runtime, 0, 1, 10, "session-b", None)
            .expect_err("second waiter must exceed configured bound");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_CALL_LIMIT"));
        assert!(error.contains("runtime.resources.maxBridgeCalls"));
    }

    #[test]
    fn bridge_registry_accounts_and_releases_settled_call_reservations() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 8, 8);
        let waiter = registry
            .register_sync(&runtime, 2, 4, 70, "session-a", Some(7))
            .expect("reserve bridge call admission");

        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 1);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 4);

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 70,
                    status: 0,
                    payload: vec![0xA7; 4],
                    reservation: None,
                },
            )
            .expect("settle admitted response");
        let response = waiter.recv().expect("receive response");
        assert_eq!(response.payload.len(), 4);
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 0);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 0);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 4);
        drop(response);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 0);

        let metrics = runtime.metrics().snapshot();
        assert!(
            metrics.resources[agentos_runtime::metrics::ResourceMetricClass::BridgeCalls.index()]
                .high_water
                >= 1
        );
        assert!(
            metrics.buffers[agentos_runtime::metrics::BufferMetricClass::Bridge.index()].high_water
                >= 4
        );
    }

    #[test]
    fn bridge_registry_delivery_failure_consumes_route_and_releases_accounting() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 8, 8);
        let waiter = registry
            .register_sync(&runtime, 2, 4, 75, "session-a", Some(7))
            .expect("register route whose receiver will be cancelled");
        drop(waiter);

        let error = registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 75,
                    status: 0,
                    payload: vec![0xA7; 4],
                    reservation: None,
                },
            )
            .expect_err("disconnected response target must reject delivery");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY"));
        assert_eq!(registry.pending_len(), 0);
        assert!(resources.is_zero());

        let waiter = registry
            .register_sync(&runtime, 2, 4, 76, "session-a", Some(7))
            .expect("register route whose terminal error cannot be delivered");
        drop(waiter);
        let error = registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 76,
                    status: 0,
                    payload: vec![0; 5],
                    reservation: None,
                },
            )
            .expect_err("terminal oversize error delivery must fail closed");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_DELIVERY"));
        assert_eq!(registry.pending_len(), 0);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_rejects_producer_side_response_reservations() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 8, 1024);
        let waiter = registry
            .register_sync(&runtime, 1, 512, 77, "session-a", Some(7))
            .expect("pre-reserve declared response maximum");
        let producer_reservation = resources
            .reserve(ResourceClass::BridgeResponseBytes, 2)
            .expect("temporary producer encoding charge");
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            514
        );

        let error = registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 77,
                    status: 0,
                    payload: vec![0xA7; 2],
                    reservation: Some(agentos_runtime::accounting::SharedReservation::new(
                        producer_reservation,
                    )),
                },
            )
            .expect_err("producer-side response charging must fail closed");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_ACCOUNTING"));

        let terminal = waiter.recv().expect("receive accounting error");
        assert_eq!(terminal.status, 1);
        assert!(String::from_utf8_lossy(&terminal.payload)
            .contains("ERR_AGENTOS_BRIDGE_RESPONSE_ACCOUNTING"));
        drop(terminal);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_bounds_and_accounts_terminal_error_payloads() {
        let registry = BridgeCallRegistry::new(1);
        let (runtime, resources) = limited_bridge_runtime(1, 1, 4);
        let waiter = registry
            .register_sync(&runtime, 0, 4, 78, "session-a", Some(7))
            .expect("reserve a deliberately tiny response budget");

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 78,
                    status: 0,
                    payload: vec![0; 5],
                    reservation: None,
                },
            )
            .expect_err("oversize response must settle a bounded terminal error");

        let terminal = waiter.recv().expect("receive bounded terminal error");
        assert_eq!(terminal.status, 1);
        assert!(terminal.payload.len() <= 4);
        assert_eq!(
            terminal.reservation.as_ref().unwrap().amount(),
            terminal.payload.len()
        );
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            terminal.payload.len()
        );
        assert!(
            terminal.payload.is_empty(),
            "a budget too small for the typed envelope must not emit a truncated string sentinel"
        );
        drop(terminal);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_limit_cancel_and_oversize_paths_leave_zero_accounting() {
        let registry = BridgeCallRegistry::new(4);
        let (runtime, resources) = limited_bridge_runtime(1, 4, 512);
        let _waiter = registry
            .register_sync(&runtime, 2, 4, 80, "session-a", Some(7))
            .expect("reserve first bridge call");

        let error = registry
            .register_sync(&runtime, 1, 1, 81, "session-a", Some(7))
            .expect_err("second call must exceed VM call limit");
        assert!(error.contains("limits.reactor.maxBridgeCalls"));
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 1);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 4);

        registry.cancel(80);
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 0);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 0);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 0);

        let error = match registry.register_sync(&runtime, 5, 1, 81, "session-a", Some(7)) {
            Ok(_) => panic!("oversize request must fail admission"),
            Err(error) => error,
        };
        assert!(error.contains("limits.reactor.maxBridgeRequestBytes"));
        assert!(resources.is_zero());

        let error = match registry.register_sync(&runtime, 2, 513, 81, "session-a", Some(7)) {
            Ok(_) => panic!("oversize declared response must fail admission"),
            Err(error) => error,
        };
        assert!(error.contains("limits.reactor.maxBridgeResponseBytes"));
        assert!(resources.is_zero());

        let oversize_waiter = registry
            .register_sync(&runtime, 2, 512, 82, "session-a", Some(7))
            .expect("reserve bridge call for oversize response");
        let error = registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 82,
                    status: 0,
                    payload: vec![0; 513],
                    reservation: None,
                },
            )
            .expect_err("oversize response must fail closed");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT"));
        let terminal = oversize_waiter
            .recv()
            .expect("oversize response must settle its waiter with an error");
        assert_eq!(terminal.status, 1);
        assert!(String::from_utf8_lossy(&terminal.payload)
            .contains("ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT"));
        assert_eq!(
            terminal.reservation.as_ref().unwrap().amount(),
            terminal.payload.len()
        );
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 0);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 0);
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            terminal.payload.len()
        );
        drop(terminal);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 0);

        let _waiter = registry
            .register_sync(&runtime, 1, 2, 83, "session-a", Some(7))
            .expect("reserve bridge call for teardown");
        registry.cancel_session("session-a", Some(7));
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_pre_reserves_overlapping_declared_maxima_until_consumed() {
        let registry = BridgeCallRegistry::new(4);
        let (runtime, resources) = limited_bridge_runtime(4, 16, 8);
        let first = registry
            .register_sync(&runtime, 1, 4, 90, "session-a", Some(7))
            .expect("pre-reserve first declared maximum");
        let second = registry
            .register_sync(&runtime, 1, 4, 91, "session-a", Some(7))
            .expect("pre-reserve second declared maximum");
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 8);

        let error = registry
            .register_sync(&runtime, 1, 1, 92, "session-a", Some(7))
            .expect_err("a call without guaranteed response capacity must fail admission");
        assert!(error.contains("limits.reactor.maxBridgeResponseBytes"));
        assert_eq!(registry.pending_len(), 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 8);

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 90,
                    status: 0,
                    payload: vec![0xA5],
                    reservation: None,
                },
            )
            .expect("transfer first pre-reservation to its completion");
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 5);

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 91,
                    status: 0,
                    payload: vec![0xB6; 2],
                    reservation: None,
                },
            )
            .expect("every admitted response must complete without reacquiring capacity");
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 3);

        let first_response = first.recv().expect("first completion");
        let second_response = second.recv().expect("second completion");
        assert_eq!(first_response.reservation.as_ref().unwrap().amount(), 1);
        assert_eq!(second_response.reservation.as_ref().unwrap().amount(), 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 3);
        drop(first_response);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 2);
        drop(second_response);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_large_declared_responses_do_not_serialize_small_completions() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 2, 16 * 1024 * 1024);
        let first = registry
            .register_sync(&runtime, 1, 16 * 1024 * 1024, 93, "session-a", Some(7))
            .expect("admit first call with a large declared response maximum");
        let second = registry
            .register_sync(&runtime, 1, 16 * 1024 * 1024, 94, "session-a", Some(7))
            .expect("a large declared maximum must not monopolize concrete response capacity");
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            8 * 1024,
            "each admitted call keeps only its bounded terminal-response reservation"
        );

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 93,
                    status: 0,
                    payload: vec![0xA5],
                    reservation: None,
                },
            )
            .expect("settle first small concrete response");
        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 94,
                    status: 0,
                    payload: vec![0xB6; 2],
                    reservation: None,
                },
            )
            .expect("settle second small concrete response");

        let first_response = first.recv().expect("first completion");
        let second_response = second.recv().expect("second completion");
        assert_eq!(first_response.payload, vec![0xA5]);
        assert_eq!(second_response.payload, vec![0xB6; 2]);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 3);
        drop(first_response);
        drop(second_response);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_grows_floor_to_concrete_response_size_before_delivery() {
        let registry = BridgeCallRegistry::new(1);
        let (runtime, resources) = limited_bridge_runtime(1, 1, 8 * 1024);
        let waiter = registry
            .register_sync(&runtime, 1, 8 * 1024, 95, "session-a", Some(7))
            .expect("admit response with the bounded terminal floor");
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            BRIDGE_TERMINAL_RESPONSE_RESERVATION_BYTES
        );

        registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 95,
                    status: 0,
                    payload: vec![0xA5; 6 * 1024],
                    reservation: None,
                },
            )
            .expect("grow concrete response reservation before delivery");
        let response = waiter.recv().expect("receive grown response");
        assert_eq!(response.payload.len(), 6 * 1024);
        assert_eq!(response.reservation.as_ref().unwrap().amount(), 6 * 1024);
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            6 * 1024
        );
        drop(response);
        assert!(resources.is_zero());
    }

    #[test]
    fn bridge_registry_growth_failure_delivers_bounded_error_and_releases_floor() {
        let registry = BridgeCallRegistry::new(2);
        let (runtime, resources) = limited_bridge_runtime(2, 2, 8 * 1024);
        let first = registry
            .register_sync(&runtime, 1, 8 * 1024, 96, "session-a", Some(7))
            .expect("admit first response floor");
        let _second = registry
            .register_sync(&runtime, 1, 8 * 1024, 97, "session-a", Some(7))
            .expect("admit second response floor");
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            8 * 1024
        );

        let error = registry
            .settle(
                "session-a",
                Some(7),
                BridgeResponse {
                    call_id: 96,
                    status: 0,
                    payload: vec![0xA5; 5 * 1024],
                    reservation: None,
                },
            )
            .expect_err("overlapping floors must prevent unbounded concrete growth");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT"));
        let terminal = first.recv().expect("receive bounded growth error");
        assert_eq!(terminal.status, 1);
        assert!(String::from_utf8_lossy(&terminal.payload)
            .contains("ERR_AGENTOS_BRIDGE_RESPONSE_LIMIT"));
        assert_eq!(
            terminal.reservation.as_ref().unwrap().amount(),
            terminal.payload.len()
        );
        assert_eq!(registry.pending_len(), 1);
        drop(terminal);
        assert_eq!(
            resources.usage(ResourceClass::BridgeResponseBytes).used,
            BRIDGE_TERMINAL_RESPONSE_RESERVATION_BYTES
        );
        registry.cancel(97);
        assert!(resources.is_zero());
    }

    #[test]
    fn async_bridge_admission_happens_before_host_visibility() {
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(4));
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(4);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(7))),
            String::from("session-a"),
            Some(7),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(1)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        let first = ctx
            .prepare_async_call_with_max_response_bytes("_first", Vec::new(), 4)
            .expect("admit first async bridge call");
        assert!(
            event_rx.is_empty(),
            "registration must not make the request host-visible"
        );
        ctx.dispatch_async_call(first)
            .expect("dispatch admitted bridge call");
        assert_eq!(event_rx.len(), 1);

        let error = match ctx.prepare_async_call_with_max_response_bytes("_second", Vec::new(), 1) {
            Ok(_) => panic!("VM call admission must fail before dispatch"),
            Err(error) => error,
        };
        assert!(error.contains("limits.reactor.maxBridgeCalls"));
        assert_eq!(
            event_rx.len(),
            1,
            "rejected call must never reach the host event lane"
        );

        drop(ctx);
        assert!(registry.pending_len() == 0);
        assert_ledger_settles_to_zero(&resources);
    }

    #[test]
    fn kernel_stdin_read_uses_session_lifetime_instead_of_operation_deadline() {
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(4));
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(7))),
            String::from("session-stdin"),
            Some(7),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(1)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            Duration::from_millis(10),
        );

        let prepared = ctx
            .prepare_async_call_with_max_response_bytes("_kernelStdinRead", Vec::new(), 4)
            .expect("admit durable stdin readiness wait");
        let call_id = prepared.call_id;
        ctx.dispatch_async_call(prepared)
            .expect("publish durable stdin readiness wait");
        event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("host receives stdin readiness wait");

        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(registry.pending_len(), 1);
        assert!(
            !registry
                .timeout(call_id)
                .expect("session-lifetime route has no operation deadline"),
            "operation deadline must not retire a durable stdin readiness wait"
        );

        registry
            .settle(
                "session-stdin",
                Some(7),
                BridgeResponse {
                    call_id,
                    status: 0,
                    payload: vec![0xA5],
                    reservation: None,
                },
            )
            .expect("stdin data settles the durable wait");
        drop(
            async_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("guest receives stdin data"),
        );
        assert_ledger_settles_to_zero(&resources);
    }

    #[test]
    fn dropping_prepared_async_call_cancels_route_and_releases_accounting() {
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(4));
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(7))),
            String::from("session-a"),
            Some(7),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(1)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        let prepared = ctx
            .prepare_async_call_with_max_response_bytes("_cancelled", vec![0xA5; 2], 4)
            .expect("admit prepared async bridge call");
        assert_eq!(registry.pending_len(), 1);
        assert_eq!(resources.usage(ResourceClass::BridgeCalls).used, 1);
        assert_eq!(resources.usage(ResourceClass::BridgeRequestBytes).used, 2);
        assert_eq!(resources.usage(ResourceClass::BridgeResponseBytes).used, 4);
        assert!(event_rx.is_empty());

        drop(prepared);
        assert_eq!(registry.pending_len(), 0);
        assert_ledger_settles_to_zero(&resources);
        assert!(event_rx.is_empty());
    }

    #[test]
    fn failed_async_dispatch_cancels_route_and_releases_accounting() {
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(4));
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(RejectingRuntimeEventSender),
            String::from("session-a"),
            Some(7),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(1)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        let prepared = ctx
            .prepare_async_call_with_max_response_bytes("_dispatch", vec![0xA5; 2], 4)
            .expect("admit prepared async bridge call");
        assert_eq!(registry.pending_len(), 1);
        let error = ctx
            .dispatch_async_call(prepared)
            .expect_err("injected event-lane failure must reject dispatch");
        assert!(error.contains("injected event-lane failure"));
        assert_eq!(registry.pending_len(), 0);
        assert_ledger_settles_to_zero(&resources);
    }

    #[test]
    fn rejected_deadline_task_cancels_unpublished_route_and_releases_accounting() {
        let (runtime, resources) = limited_bridge_runtime(1, 4, 4);
        runtime.close_admission();
        let registry: CallIdRouter = Arc::new(BridgeCallRegistry::new(4));
        let (event_tx, event_rx) = crossbeam_channel::unbounded();
        let (async_tx, _async_rx) = crossbeam_channel::bounded(1);
        let (_abort_tx, abort_rx) = crossbeam_channel::bounded(1);
        let ctx = BridgeCallContext::with_registry(
            Box::new(ChannelRuntimeEventSender::new(event_tx, Some(7))),
            String::from("session-a"),
            Some(7),
            Arc::clone(&registry),
            Arc::new(AtomicU64::new(1)),
            async_tx,
            abort_rx,
            runtime,
            Arc::new(crate::session::SessionPauseControl::default()),
            DEFAULT_BRIDGE_CALL_TIMEOUT,
        );

        let error = match ctx.prepare_async_call_with_max_response_bytes(
            "_timer-rejected",
            vec![0xA5; 2],
            4,
        ) {
            Ok(_) => panic!("closed task admission must reject the deadline task"),
            Err(error) => error,
        };
        assert!(error.contains("ERR_AGENTOS_BRIDGE_DEADLINE_TASK"));
        assert_eq!(registry.pending_len(), 0);
        assert!(event_rx.is_empty());
        assert!(resources.is_zero());
    }

    #[test]
    fn async_bridge_admission_reserves_every_response_lane_slot() {
        let (runtime, resources) = limited_bridge_runtime(3, 8, 8);
        let registry = BridgeCallRegistry::new(4);
        let (response_tx, response_rx) = crossbeam_channel::bounded(2);

        for call_id in [101, 102] {
            registry
                .register_async(
                    &runtime,
                    1,
                    1,
                    call_id,
                    "session-a",
                    Some(7),
                    response_tx.clone(),
                )
                .expect("each physical response slot admits one async call");
        }
        let error = registry
            .register_async(
                &runtime,
                1,
                1,
                103,
                "session-a",
                Some(7),
                response_tx.clone(),
            )
            .expect_err("an async call without a response slot must fail admission");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_LANE_LIMIT"));

        for call_id in [101, 102] {
            registry
                .settle(
                    "session-a",
                    Some(7),
                    BridgeResponse {
                        call_id,
                        status: 0,
                        payload: vec![call_id as u8],
                        reservation: None,
                    },
                )
                .expect("every admitted response must fit before the lane drains");
        }
        let error = registry
            .register_async(
                &runtime,
                1,
                1,
                103,
                "session-a",
                Some(7),
                response_tx.clone(),
            )
            .expect_err("queued responses must continue to own their lane slots");
        assert!(error.contains("ERR_AGENTOS_BRIDGE_RESPONSE_LANE_LIMIT"));

        drop(
            response_rx
                .recv()
                .expect("drain one reserved response slot"),
        );
        registry
            .register_async(&runtime, 1, 1, 103, "session-a", Some(7), response_tx)
            .expect("draining a response releases exactly one admission slot");
        registry.cancel(103);
        drop(response_rx.recv().expect("drain remaining response"));
        assert!(resources.is_zero());
    }

    #[test]
    fn writer_runtime_event_sender_serializes_events() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let sender = super::ChannelRuntimeEventSender::new(tx, None);

        // Send multiple frames — buffer grows to high-water mark
        for i in 0..5 {
            let event = RuntimeEvent::BridgeCall {
                session_id: "sess-1".into(),
                call_id: i,
                method: "_fn".into(),
                payload: vec![0xAA; 100 * (i as usize + 1)],
            };
            sender.send_event(event).expect("send_event");
        }

        // Verify all events arrive with their payload intact.
        for i in 0..5u64 {
            let decoded = rx.recv().expect("recv");
            match decoded.event {
                RuntimeEvent::BridgeCall {
                    call_id, payload, ..
                } => {
                    assert_eq!(call_id, i);
                    assert_eq!(payload.len(), 100 * (i as usize + 1));
                }
                _ => panic!("expected BridgeCall"),
            }
        }

        // Small follow-up events still go through the same sender.
        let small = RuntimeEvent::Log {
            session_id: "s".into(),
            channel: 0,
            message: "x".into(),
        };
        sender.send_event(small.clone()).expect("send_event");
        let decoded = rx.recv().expect("recv");
        assert_eq!(decoded.event, small);
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
