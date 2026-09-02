mod generated;
mod versioned;

#[doc(hidden)]
pub mod protocol {
    pub use crate::generated::v1::*;

    pub mod versioned {
        pub use crate::versioned::*;
    }
}

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use agentos_runtime::{RuntimeContext, TaskClass};
use generated::v1 as wire;
pub use generated::v1::SqlValue;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch, Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::{timeout, timeout_at, Instant};
use vbare::OwnedVersionedData;

const PROTOCOL_VERSION: u16 = 1;
const MAX_FRAME_BYTES: u32 = 32 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const DEFAULT_MAX_IN_FLIGHT_REQUESTS: usize = 64;
pub const DEFAULT_MAX_QUEUED_REQUEST_BYTES: usize = 64 * 1024 * 1024;
const REQUEST_BASE_CHARGE_BYTES: usize = 256;
const REQUEST_VALUE_CHARGE_BYTES: usize = 32;

#[derive(Debug, Clone)]
pub struct ActorUdsClientConfig {
    pub request_timeout: Duration,
    pub max_in_flight_requests: usize,
    pub max_queued_request_bytes: usize,
}

impl Default for ActorUdsClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_in_flight_requests: DEFAULT_MAX_IN_FLIGHT_REQUESTS,
            max_queued_request_bytes: DEFAULT_MAX_QUEUED_REQUEST_BYTES,
        }
    }
}

#[derive(Debug, Error)]
pub enum ActorUdsError {
    #[error("actor SQLite UDS I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("actor SQLite UDS protocol failed: {0}")]
    Protocol(String),
    #[error("actor SQLite UDS protocol version is unsupported")]
    VersionMismatch,
    #[error("actor SQLite UDS endpoint closed")]
    EndpointClosed,
    #[error("actor SQLite UDS queue limit {limit} reached (capacity {capacity})")]
    QueueFull { limit: String, capacity: u32 },
    #[error("actor SQLite UDS transaction lease is invalid: {message}")]
    InvalidLeaseKey { message: String },
    #[error("actor SQLite UDS transaction lease expired after {timeout_ms}ms: {message}")]
    LeaseExpired { timeout_ms: u64, message: String },
    #[error("actor SQLite UDS response exceeded the negotiated frame limit")]
    ResponseTooLarge,
    #[error("actor SQLite UDS request used {used} bytes, frame limit is {limit}")]
    RequestTooLarge { used: usize, limit: usize },
    #[error(
        "actor SQLite UDS client used {used} of {limit} {setting}; raise {setting} to allow more"
    )]
    ClientLimit {
        setting: &'static str,
        used: usize,
        limit: usize,
    },
    #[error("actor SQLite UDS connection generation closed: {message}")]
    ConnectionClosed { message: Arc<str> },
    #[error("actor SQLite UDS task admission failed: {0}")]
    TaskAdmission(String),
    #[error("actor SQLite UDS client configuration is invalid: {0}")]
    InvalidConfig(String),
    #[error("actor SQLite UDS transaction handle is already terminal")]
    TransactionTerminal,
    #[error("actor SQLite error {code} at statement {statement_index}: {message}")]
    Sql {
        code: i32,
        statement_index: u32,
        message: String,
    },
    #[error("actor SQLite UDS {operation} timed out after {timeout_ms}ms")]
    Timeout {
        operation: &'static str,
        timeout_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
    pub changes: i64,
    pub last_insert_row_id: Option<i64>,
}

#[derive(Clone)]
pub struct ActorUdsClient {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    runtime: RuntimeContext,
    config: ActorUdsClientConfig,
    next_generation: AtomicU64,
    connection: Mutex<Option<Arc<Connection>>>,
}

struct Connection {
    request_timeout: Duration,
    commands: mpsc::Sender<RequestCommand>,
    terminate: watch::Sender<Option<Arc<str>>>,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
    in_flight: Arc<Semaphore>,
    queued_bytes: Arc<Semaphore>,
    max_in_flight_requests: usize,
    max_queued_request_bytes: usize,
    in_flight_warning_active: AtomicBool,
    queued_bytes_warning_active: AtomicBool,
    driver: StdMutex<Option<JoinHandle<()>>>,
}

struct RequestCommand {
    lease_key: Option<String>,
    payload: wire::RequestPayload,
    deadline: Instant,
    response: oneshot::Sender<Result<wire::ResponsePayload, ActorUdsError>>,
    in_flight: OwnedSemaphorePermit,
    queued_bytes: OwnedSemaphorePermit,
}

struct PendingRequest {
    deadline: Instant,
    response: oneshot::Sender<Result<wire::ResponsePayload, ActorUdsError>>,
    _in_flight: OwnedSemaphorePermit,
}

struct OutboundRequest {
    request_id: u32,
    lease_key: Option<String>,
    payload: wire::RequestPayload,
    _queued_bytes: OwnedSemaphorePermit,
}

enum ReaderEvent {
    Response(wire::Response),
    Terminal(Arc<str>),
}

enum WriterEvent {
    Written(u32),
    RequestFailed(u32, ActorUdsError),
    Terminal(Arc<str>),
}

struct DriverLifecycle {
    generation: u64,
    terminate: watch::Sender<Option<Arc<str>>>,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
}

impl Drop for DriverLifecycle {
    fn drop(&mut self) {
        self.terminate.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(Arc::from(format!(
                    "connection driver cancelled on generation {}",
                    self.generation
                )));
                true
            }
        });
        self.closed.store(true, Ordering::Release);
        self.closed_notify.notify_waiters();
    }
}

struct AbortTaskOnDrop(Option<JoinHandle<()>>);

impl AbortTaskOnDrop {
    fn new(task: JoinHandle<()>) -> Self {
        Self(Some(task))
    }

    async fn abort_and_wait(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for AbortTaskOnDrop {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

pub struct ActorUdsTransaction {
    client: ActorUdsClient,
    connection: Arc<Connection>,
    lease_key: String,
    active: bool,
}

impl ActorUdsClient {
    pub fn new(
        path: impl Into<PathBuf>,
        runtime: RuntimeContext,
        config: ActorUdsClientConfig,
    ) -> Result<Self, ActorUdsError> {
        if config.request_timeout.is_zero() {
            return Err(ActorUdsError::InvalidConfig(String::from(
                "request_timeout must be positive",
            )));
        }
        if config.max_in_flight_requests == 0
            || config.max_in_flight_requests > Semaphore::MAX_PERMITS
        {
            return Err(ActorUdsError::InvalidConfig(format!(
                "limits.sqlite.maxInFlightRequests must be between 1 and {}",
                Semaphore::MAX_PERMITS
            )));
        }
        if config.max_queued_request_bytes == 0
            || config.max_queued_request_bytes > Semaphore::MAX_PERMITS
        {
            return Err(ActorUdsError::InvalidConfig(format!(
                "limits.sqlite.maxQueuedRequestBytes must be between 1 and {}",
                Semaphore::MAX_PERMITS
            )));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                path: path.into(),
                runtime,
                config,
                next_generation: AtomicU64::new(1),
                connection: Mutex::new(None),
            }),
        })
    }

    pub async fn exec(&self, script: impl Into<String>) -> Result<(), ActorUdsError> {
        match self
            .request(
                wire::RequestPayload::SqliteExec(wire::SqliteExec {
                    script: script.into(),
                }),
                None,
            )
            .await?
        {
            wire::ResponsePayload::SqliteExecOk => Ok(()),
            other => Err(unexpected_response("exec", &other)),
        }
    }

    pub async fn query(
        &self,
        sql: impl Into<String>,
        params: Vec<SqlValue>,
    ) -> Result<QueryResult, ActorUdsError> {
        self.query_with_lease(sql, params, None).await
    }

    async fn query_with_lease(
        &self,
        sql: impl Into<String>,
        params: Vec<SqlValue>,
        lease_key: Option<&str>,
    ) -> Result<QueryResult, ActorUdsError> {
        let connection = self.connection().await?;
        self.query_on(connection, sql.into(), params, lease_key.map(str::to_owned))
            .await
    }

    async fn query_on(
        &self,
        connection: Arc<Connection>,
        sql: String,
        params: Vec<SqlValue>,
        lease_key: Option<String>,
    ) -> Result<QueryResult, ActorUdsError> {
        match self
            .request_on(
                connection,
                wire::RequestPayload::SqliteQuery(wire::SqliteQuery { sql, params }),
                lease_key,
            )
            .await?
        {
            wire::ResponsePayload::SqliteQueryOk(result) => Ok(QueryResult {
                columns: result.columns,
                rows: result.rows,
                changes: result.changes,
                last_insert_row_id: result.last_insert_row_id,
            }),
            other => Err(unexpected_response("query", &other)),
        }
    }

    pub async fn begin(
        &self,
        lease_key: impl Into<String>,
        timeout_ms: Option<u64>,
    ) -> Result<ActorUdsTransaction, ActorUdsError> {
        let lease_key = lease_key.into();
        let connection = self.connection().await?;
        match self
            .request_on(
                Arc::clone(&connection),
                wire::RequestPayload::SqliteBegin(wire::SqliteBegin {
                    lease_key: lease_key.clone(),
                    timeout_ms,
                }),
                None,
            )
            .await?
        {
            wire::ResponsePayload::SqliteBeginOk => Ok(ActorUdsTransaction {
                client: self.clone(),
                connection,
                lease_key,
                active: true,
            }),
            other => Err(unexpected_response("begin", &other)),
        }
    }

    pub fn request_timeout_ms(&self) -> u64 {
        self.inner
            .config
            .request_timeout
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    async fn request(
        &self,
        payload: wire::RequestPayload,
        lease_key: Option<String>,
    ) -> Result<wire::ResponsePayload, ActorUdsError> {
        let connection = self.connection().await?;
        self.request_on(connection, payload, lease_key).await
    }

    async fn request_on(
        &self,
        connection: Arc<Connection>,
        payload: wire::RequestPayload,
        lease_key: Option<String>,
    ) -> Result<wire::ResponsePayload, ActorUdsError> {
        if connection.is_closed() {
            return Err(connection.closed_error());
        }

        let charge = request_charge_bytes(&payload, lease_key.as_deref())?;
        let in_flight = Arc::clone(&connection.in_flight)
            .try_acquire_owned()
            .map_err(|_| ActorUdsError::ClientLimit {
                setting: "limits.sqlite.maxInFlightRequests",
                used: connection.max_in_flight_requests - connection.in_flight.available_permits()
                    + 1,
                limit: connection.max_in_flight_requests,
            })?;
        let queued_bytes = Arc::clone(&connection.queued_bytes)
            .try_acquire_many_owned(charge)
            .map_err(|_| ActorUdsError::ClientLimit {
                setting: "limits.sqlite.maxQueuedRequestBytes",
                used: connection.max_queued_request_bytes
                    - connection.queued_bytes.available_permits()
                    + charge as usize,
                limit: connection.max_queued_request_bytes,
            })?;
        connection.observe_limits();
        let deadline = Instant::now() + connection.request_timeout;
        let (response, receiver) = oneshot::channel();
        let command = RequestCommand {
            lease_key,
            payload,
            deadline,
            response,
            in_flight,
            queued_bytes,
        };
        match connection.commands.try_send(command) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(command)) => {
                drop(command);
                return Err(ActorUdsError::ClientLimit {
                    setting: "limits.sqlite.maxInFlightRequests",
                    used: connection.max_in_flight_requests + 1,
                    limit: connection.max_in_flight_requests,
                });
            }
            Err(mpsc::error::TrySendError::Closed(command)) => {
                drop(command);
                return Err(connection.closed_error());
            }
        }
        match timeout_at(deadline, receiver).await {
            Ok(Ok(result)) => {
                if matches!(result, Err(ActorUdsError::Timeout { .. })) {
                    connection.wait_closed().await;
                }
                result
            }
            Ok(Err(_)) => Err(connection.closed_error()),
            Err(_) => {
                connection.terminate(Arc::from("request deadline expired"));
                connection.wait_closed().await;
                Err(ActorUdsError::Timeout {
                    operation: "request",
                    timeout_ms: connection
                        .request_timeout
                        .as_millis()
                        .min(u128::from(u64::MAX)) as u64,
                })
            }
        }
    }

    async fn connection(&self) -> Result<Arc<Connection>, ActorUdsError> {
        let mut slot = self.inner.connection.lock().await;
        if let Some(connection) = slot.as_ref() {
            if !connection.is_closed() {
                return Ok(Arc::clone(connection));
            }
        }
        if let Some(connection) = slot.take() {
            connection.wait_closed().await;
        }
        let connected = connect(&self.inner.path).await?;
        let generation = self
            .inner
            .next_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .map_err(|_| {
                ActorUdsError::Protocol(String::from("connection generation space exhausted"))
            })?;
        let connection = Connection::start(
            generation,
            connected,
            self.inner.runtime.clone(),
            self.inner.config.clone(),
        )
        .await?;
        *slot = Some(Arc::clone(&connection));
        Ok(connection)
    }
}

impl ActorUdsTransaction {
    pub async fn query(
        &self,
        sql: impl Into<String>,
        params: Vec<SqlValue>,
    ) -> Result<QueryResult, ActorUdsError> {
        self.ensure_active()?;
        self.client
            .query_on(
                Arc::clone(&self.connection),
                sql.into(),
                params,
                Some(self.lease_key.clone()),
            )
            .await
    }

    pub async fn commit(&mut self) -> Result<(), ActorUdsError> {
        self.ensure_active()?;
        let result = self
            .client
            .request_on(
                Arc::clone(&self.connection),
                wire::RequestPayload::SqliteCommit(wire::SqliteCommit {
                    lease_key: self.lease_key.clone(),
                }),
                None,
            )
            .await;
        match result {
            Ok(wire::ResponsePayload::SqliteCommitOk) => {
                self.active = false;
                Ok(())
            }
            Ok(other) => Err(unexpected_response("commit", &other)),
            Err(error) => {
                if self.connection.is_closed() {
                    self.active = false;
                }
                Err(error)
            }
        }
    }

    pub async fn rollback(&mut self) -> Result<(), ActorUdsError> {
        self.ensure_active()?;
        let result = self
            .client
            .request_on(
                Arc::clone(&self.connection),
                wire::RequestPayload::SqliteRollback(wire::SqliteRollback {
                    lease_key: self.lease_key.clone(),
                }),
                None,
            )
            .await;
        match result {
            Ok(wire::ResponsePayload::SqliteRollbackOk) => {
                self.active = false;
                Ok(())
            }
            Ok(other) => Err(unexpected_response("rollback", &other)),
            Err(error) => {
                if self.connection.is_closed() {
                    self.active = false;
                }
                Err(error)
            }
        }
    }

    fn ensure_active(&self) -> Result<(), ActorUdsError> {
        if !self.active {
            return Err(ActorUdsError::TransactionTerminal);
        }
        if self.connection.is_closed() {
            return Err(self.connection.closed_error());
        }
        Ok(())
    }
}

struct Connected {
    stream: UnixStream,
    max_frame_bytes: u32,
}

impl Connection {
    async fn start(
        generation: u64,
        connected: Connected,
        runtime: RuntimeContext,
        config: ActorUdsClientConfig,
    ) -> Result<Arc<Self>, ActorUdsError> {
        let (read_half, write_half) = connected.stream.into_split();
        let (commands, command_receiver) = mpsc::channel(config.max_in_flight_requests);
        let (outbound, outbound_receiver) = mpsc::channel(config.max_in_flight_requests);
        let (reader_events, reader_event_receiver) = mpsc::channel(1);
        let (writer_events, writer_event_receiver) = mpsc::channel(config.max_in_flight_requests);
        let (terminate, terminate_receiver) = watch::channel(None);
        let closed = Arc::new(AtomicBool::new(false));
        let closed_notify = Arc::new(Notify::new());

        let reader = runtime
            .spawn(TaskClass::Runtime, async move {
                reader_loop(read_half, connected.max_frame_bytes, reader_events).await;
            })
            .map_err(|error| ActorUdsError::TaskAdmission(error.to_string()))?;
        let reader = AbortTaskOnDrop::new(reader);
        let writer = runtime
            .spawn(TaskClass::Runtime, async move {
                writer_loop(
                    write_half,
                    connected.max_frame_bytes,
                    outbound_receiver,
                    writer_events,
                    generation,
                )
                .await;
            })
            .map_err(|error| ActorUdsError::TaskAdmission(error.to_string()))?;
        let writer = AbortTaskOnDrop::new(writer);
        let driver_closed = Arc::clone(&closed);
        let driver_closed_notify = Arc::clone(&closed_notify);
        let driver_terminate = terminate.clone();
        let request_timeout = config.request_timeout;
        let driver_runtime = runtime.clone();
        let driver = match runtime.spawn(TaskClass::Runtime, async move {
            driver_loop(
                generation,
                command_receiver,
                outbound,
                reader_event_receiver,
                writer_event_receiver,
                terminate_receiver,
                driver_terminate,
                driver_closed,
                driver_closed_notify,
                reader,
                writer,
                request_timeout,
                driver_runtime,
            )
            .await;
        }) {
            Ok(driver) => driver,
            Err(error) => {
                return Err(ActorUdsError::TaskAdmission(error.to_string()));
            }
        };

        Ok(Arc::new(Self {
            request_timeout: config.request_timeout,
            commands,
            terminate,
            closed,
            closed_notify,
            in_flight: Arc::new(Semaphore::new(config.max_in_flight_requests)),
            queued_bytes: Arc::new(Semaphore::new(config.max_queued_request_bytes)),
            max_in_flight_requests: config.max_in_flight_requests,
            max_queued_request_bytes: config.max_queued_request_bytes,
            in_flight_warning_active: AtomicBool::new(false),
            queued_bytes_warning_active: AtomicBool::new(false),
            driver: StdMutex::new(Some(driver)),
        }))
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn terminate(&self, reason: Arc<str>) {
        self.terminate.send_if_modified(|current| {
            if current.is_some() {
                false
            } else {
                *current = Some(reason);
                true
            }
        });
    }

    fn closed_error(&self) -> ActorUdsError {
        let message = self
            .terminate
            .borrow()
            .clone()
            .unwrap_or_else(|| Arc::from("connection driver stopped"));
        ActorUdsError::ConnectionClosed { message }
    }

    fn observe_limits(&self) {
        observe_limit(
            "limits.sqlite.maxInFlightRequests",
            self.max_in_flight_requests - self.in_flight.available_permits(),
            self.max_in_flight_requests,
            &self.in_flight_warning_active,
        );
        observe_limit(
            "limits.sqlite.maxQueuedRequestBytes",
            self.max_queued_request_bytes - self.queued_bytes.available_permits(),
            self.max_queued_request_bytes,
            &self.queued_bytes_warning_active,
        );
    }

    async fn wait_closed(&self) {
        let driver = self.driver.lock().ok().and_then(|mut driver| driver.take());
        if let Some(driver) = driver {
            let _ = driver.await;
            return;
        }
        while !self.is_closed() {
            let notified = self.closed_notify.notified();
            if self.is_closed() {
                break;
            }
            notified.await;
        }
    }
}

fn observe_limit(setting: &'static str, used: usize, limit: usize, warning_active: &AtomicBool) {
    let warning_threshold = limit.saturating_sub(limit / 5);
    if used >= warning_threshold {
        if !warning_active.swap(true, Ordering::AcqRel) {
            eprintln!(
                "WARN_AGENTOS_SQLITE_UDS_LIMIT: {setting} used {used} of {limit}; raise {setting} if this workload needs more concurrency"
            );
        }
    } else {
        warning_active.store(false, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
async fn driver_loop(
    generation: u64,
    mut commands: mpsc::Receiver<RequestCommand>,
    outbound: mpsc::Sender<OutboundRequest>,
    mut reader_events: mpsc::Receiver<ReaderEvent>,
    mut writer_events: mpsc::Receiver<WriterEvent>,
    mut terminate_receiver: watch::Receiver<Option<Arc<str>>>,
    terminate: watch::Sender<Option<Arc<str>>>,
    closed: Arc<AtomicBool>,
    closed_notify: Arc<Notify>,
    reader: AbortTaskOnDrop,
    writer: AbortTaskOnDrop,
    request_timeout: Duration,
    runtime: RuntimeContext,
) {
    let lifecycle = DriverLifecycle {
        generation,
        terminate: terminate.clone(),
        closed: Arc::clone(&closed),
        closed_notify: Arc::clone(&closed_notify),
    };
    let mut pending = HashMap::<u32, PendingRequest>::new();
    let mut next_request_id = 1_u32;
    let reason = loop {
        let next_deadline = pending.values().map(|request| request.deadline).min();
        let deadline = next_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(86400));
        tokio::select! {
            biased;
            changed = terminate_receiver.changed() => {
                match changed {
                    Ok(()) => {
                        if let Some(reason) = terminate_receiver.borrow().clone() {
                            break reason;
                        }
                    }
                    Err(_) => break Arc::from("connection owner dropped"),
                }
            }
            () = runtime.admission_closed() => {
                break Arc::from(format!(
                    "runtime admission closed on generation {generation}"
                ));
            }
            event = reader_events.recv() => {
                match event {
                    Some(ReaderEvent::Response(response)) => {
                        let Some(request) = pending.remove(&response.request_id) else {
                            break Arc::from(format!(
                                "protocol violation on generation {generation}: response for unknown request {}",
                                response.request_id
                            ));
                        };
                        let _ = request.response.send(map_response(response.payload));
                    }
                    Some(ReaderEvent::Terminal(reason)) => break reason,
                    None => break Arc::from(format!(
                        "response reader stopped on generation {generation}"
                    )),
                }
            }
            event = writer_events.recv() => {
                match event {
                    Some(WriterEvent::Written(request_id)) => {
                        // A fast response may have already removed this entry.
                        let _ = pending.get(&request_id);
                    }
                    Some(WriterEvent::RequestFailed(request_id, error)) => {
                        let Some(request) = pending.remove(&request_id) else {
                            continue;
                        };
                        let _ = request.response.send(Err(error));
                    }
                    Some(WriterEvent::Terminal(reason)) => break reason,
                    None => break Arc::from(format!(
                        "request writer stopped on generation {generation}"
                    )),
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break Arc::from("connection owner dropped");
                };
                if Instant::now() >= command.deadline {
                    let _ = command.response.send(Err(ActorUdsError::Timeout {
                        operation: "request",
                        timeout_ms: request_timeout.as_millis().min(u128::from(u64::MAX)) as u64,
                    }));
                    continue;
                }
                let request_id = next_request_id;
                let Some(next) = next_request_id.checked_add(1) else {
                    let _ = command.response.send(Err(ActorUdsError::Protocol(String::from(
                        "request id space exhausted",
                    ))));
                    break Arc::from("request id space exhausted");
                };
                next_request_id = next;
                let RequestCommand {
                    lease_key,
                    payload,
                    deadline,
                    response,
                    in_flight,
                    queued_bytes,
                } = command;
                pending.insert(
                    request_id,
                    PendingRequest {
                        deadline,
                        response,
                        _in_flight: in_flight,
                    },
                );
                let outbound_request = OutboundRequest {
                    request_id,
                    lease_key,
                    payload,
                    _queued_bytes: queued_bytes,
                };
                if let Err(error) = outbound.try_send(outbound_request) {
                    let request = pending
                        .remove(&request_id)
                        .expect("pending entry exists for outbound request");
                    let _ = request.response.send(Err(ActorUdsError::ConnectionClosed {
                        message: Arc::from("request writer queue closed"),
                    }));
                    drop(error);
                    break Arc::from("request writer queue closed");
                }
            }
            _ = tokio::time::sleep_until(deadline), if next_deadline.is_some() => {
                let expired = pending
                    .iter()
                    .filter_map(|(request_id, request)| {
                        (request.deadline <= Instant::now()).then_some(*request_id)
                    })
                    .collect::<Vec<_>>();
                for request_id in expired {
                    if let Some(request) = pending.remove(&request_id) {
                        let _ = request.response.send(Err(ActorUdsError::Timeout {
                            operation: "request",
                            timeout_ms: request_timeout
                                .as_millis()
                                .min(u128::from(u64::MAX)) as u64,
                        }));
                    }
                }
                break Arc::from(format!(
                    "request deadline expired on generation {generation}"
                ));
            }
        }
    };

    terminate.send_if_modified(|current| {
        if current.is_some() {
            false
        } else {
            *current = Some(Arc::clone(&reason));
            true
        }
    });
    closed.store(true, Ordering::Release);
    closed_notify.notify_waiters();
    reader.abort_and_wait().await;
    writer.abort_and_wait().await;
    let response_error = || ActorUdsError::ConnectionClosed {
        message: Arc::clone(&reason),
    };
    for (_, request) in pending.drain() {
        let _ = request.response.send(Err(response_error()));
    }
    while let Ok(command) = commands.try_recv() {
        let _ = command.response.send(Err(response_error()));
    }
    drop(lifecycle);
}

async fn writer_loop(
    mut writer: OwnedWriteHalf,
    max_frame_bytes: u32,
    mut outbound: mpsc::Receiver<OutboundRequest>,
    events: mpsc::Sender<WriterEvent>,
    generation: u64,
) {
    while let Some(request) = outbound.recv().await {
        let request_id = request.request_id;
        let frame = wire::ClientFrame::Request(wire::Request {
            request_id,
            lease_key: request.lease_key,
            payload: request.payload,
        });
        let encoded = match versioned::ClientFrame::wrap_latest(frame)
            .serialize_with_embedded_version(PROTOCOL_VERSION)
        {
            Ok(encoded) => encoded,
            Err(error) => {
                if events
                    .send(WriterEvent::RequestFailed(
                        request_id,
                        ActorUdsError::Protocol(error.to_string()),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
        };
        if encoded.len() > max_frame_bytes as usize {
            if events
                .send(WriterEvent::RequestFailed(
                    request_id,
                    ActorUdsError::RequestTooLarge {
                        used: encoded.len(),
                        limit: max_frame_bytes as usize,
                    },
                ))
                .await
                .is_err()
            {
                return;
            }
            continue;
        }
        if let Err(error) = write_frame(&mut writer, &encoded).await {
            let _ = events
                .send(WriterEvent::Terminal(Arc::from(format!(
                    "request write failed on generation {generation}: {error}"
                ))))
                .await;
            return;
        }
        if events.send(WriterEvent::Written(request_id)).await.is_err() {
            return;
        }
    }
    let _ = events
        .send(WriterEvent::Terminal(Arc::from(format!(
            "request writer input closed on generation {generation}"
        ))))
        .await;
}

async fn reader_loop(
    mut reader: OwnedReadHalf,
    max_frame_bytes: u32,
    events: mpsc::Sender<ReaderEvent>,
) {
    let reason = loop {
        let encoded = match read_frame(&mut reader, max_frame_bytes).await {
            Ok(encoded) => encoded,
            Err(error) => break Arc::from(format!("response read failed: {error}")),
        };
        let frame = match versioned::ServerFrame::deserialize_with_embedded_version(&encoded) {
            Ok(frame) => frame,
            Err(error) => break Arc::from(format!("response decode failed: {error}")),
        };
        match frame {
            wire::ServerFrame::Response(response) => {
                if events.send(ReaderEvent::Response(response)).await.is_err() {
                    return;
                }
            }
            wire::ServerFrame::GoAway(_) => break Arc::from("server sent GoAway"),
        }
    };
    let _ = events.send(ReaderEvent::Terminal(reason)).await;
}

fn request_charge_bytes(
    payload: &wire::RequestPayload,
    lease_key: Option<&str>,
) -> Result<u32, ActorUdsError> {
    let mut used = REQUEST_BASE_CHARGE_BYTES + lease_key.map_or(0, str::len);
    match payload {
        wire::RequestPayload::SqliteExec(request) => {
            used = used.saturating_add(request.script.len())
        }
        wire::RequestPayload::SqliteQuery(request) => {
            used = used.saturating_add(request.sql.len());
            for value in &request.params {
                used = used.saturating_add(REQUEST_VALUE_CHARGE_BYTES);
                used = used.saturating_add(match value {
                    SqlValue::SqlText(value) => value.len(),
                    SqlValue::SqlBlob(value) => value.len(),
                    SqlValue::SqlNull | SqlValue::SqlInteger(_) | SqlValue::SqlReal(_) => 0,
                });
            }
        }
        wire::RequestPayload::SqliteBegin(request) => {
            used = used.saturating_add(request.lease_key.len());
        }
        wire::RequestPayload::SqliteCommit(request) => {
            used = used.saturating_add(request.lease_key.len());
        }
        wire::RequestPayload::SqliteRollback(request) => {
            used = used.saturating_add(request.lease_key.len());
        }
    }
    u32::try_from(used).map_err(|_| ActorUdsError::RequestTooLarge {
        used,
        limit: u32::MAX as usize,
    })
}

async fn connect(path: &Path) -> Result<Connected, ActorUdsError> {
    let mut stream = timeout(DEFAULT_CONNECT_TIMEOUT, UnixStream::connect(path))
        .await
        .map_err(|_| ActorUdsError::Timeout {
            operation: "connect",
            timeout_ms: DEFAULT_CONNECT_TIMEOUT.as_millis() as u64,
        })??;
    let hello = versioned::ClientHello::wrap_latest(())
        .serialize_with_embedded_version(PROTOCOL_VERSION)
        .map_err(|error| ActorUdsError::Protocol(error.to_string()))?;
    write_frame(&mut stream, &hello).await?;
    let response = read_frame(&mut stream, MAX_FRAME_BYTES).await?;
    match versioned::ServerHello::deserialize_with_embedded_version(&response)
        .map_err(|error| ActorUdsError::Protocol(error.to_string()))?
    {
        wire::ServerHello::HelloOk(ok) => Ok(Connected {
            stream,
            max_frame_bytes: ok.max_frame_bytes.min(MAX_FRAME_BYTES),
        }),
        wire::ServerHello::HelloRejectUnsupportedVersion => Err(ActorUdsError::VersionMismatch),
    }
}

async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    payload: &[u8],
) -> Result<(), ActorUdsError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| ActorUdsError::Protocol("frame length exceeds u32".to_owned()))?;
    stream.write_u32(length).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(
    stream: &mut (impl AsyncRead + Unpin),
    max_frame_bytes: u32,
) -> Result<Vec<u8>, ActorUdsError> {
    let length = stream.read_u32().await?;
    if length > max_frame_bytes {
        return Err(ActorUdsError::Protocol(format!(
            "response frame is {length} bytes, limit is {max_frame_bytes} bytes"
        )));
    }
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

fn map_response(payload: wire::ResponsePayload) -> Result<wire::ResponsePayload, ActorUdsError> {
    match payload {
        wire::ResponsePayload::SqlError(error) => Err(ActorUdsError::Sql {
            code: error.code,
            statement_index: error.statement_index,
            message: error.message,
        }),
        wire::ResponsePayload::EndpointClosed => Err(ActorUdsError::EndpointClosed),
        wire::ResponsePayload::QueueFull(error) => Err(ActorUdsError::QueueFull {
            limit: error.limit,
            capacity: error.capacity,
        }),
        wire::ResponsePayload::InvalidLeaseKey(error) => Err(ActorUdsError::InvalidLeaseKey {
            message: error.message,
        }),
        wire::ResponsePayload::LeaseExpired(error) => Err(ActorUdsError::LeaseExpired {
            timeout_ms: error.timeout_ms,
            message: error.message,
        }),
        wire::ResponsePayload::ResponseTooLarge => Err(ActorUdsError::ResponseTooLarge),
        response => Ok(response),
    }
}

fn unexpected_response(operation: &str, response: &wire::ResponsePayload) -> ActorUdsError {
    ActorUdsError::Protocol(format!("unexpected {operation} response: {response:?}"))
}
