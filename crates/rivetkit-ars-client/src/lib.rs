mod generated;
mod versioned;

#[doc(hidden)]
pub mod protocol {
    pub use crate::generated::v1::*;

    pub mod versioned {
        pub use crate::versioned::*;
    }
}

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use generated::v1 as wire;
pub use generated::v1::SqlValue;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use vbare::OwnedVersionedData;

const PROTOCOL_VERSION: u16 = 1;
const MAX_FRAME_BYTES: u32 = 32 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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
    request_timeout: Duration,
    next_request_id: AtomicU32,
    connection: Mutex<Option<Connection>>,
}

struct Connection {
    stream: UnixStream,
    max_frame_bytes: u32,
}

impl ActorUdsClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_request_timeout(path, DEFAULT_REQUEST_TIMEOUT)
    }

    pub fn with_request_timeout(path: impl Into<PathBuf>, request_timeout: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                path: path.into(),
                request_timeout,
                next_request_id: AtomicU32::new(1),
                connection: Mutex::new(None),
            }),
        }
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

    pub async fn query_with_lease(
        &self,
        sql: impl Into<String>,
        params: Vec<SqlValue>,
        lease_key: Option<&str>,
    ) -> Result<QueryResult, ActorUdsError> {
        match self
            .request(
                wire::RequestPayload::SqliteQuery(wire::SqliteQuery {
                    sql: sql.into(),
                    params,
                }),
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
    ) -> Result<(), ActorUdsError> {
        match self
            .request(
                wire::RequestPayload::SqliteBegin(wire::SqliteBegin {
                    lease_key: lease_key.into(),
                    timeout_ms,
                }),
                None,
            )
            .await?
        {
            wire::ResponsePayload::SqliteBeginOk => Ok(()),
            other => Err(unexpected_response("begin", &other)),
        }
    }

    pub async fn commit(&self, lease_key: impl Into<String>) -> Result<(), ActorUdsError> {
        match self
            .request(
                wire::RequestPayload::SqliteCommit(wire::SqliteCommit {
                    lease_key: lease_key.into(),
                }),
                None,
            )
            .await?
        {
            wire::ResponsePayload::SqliteCommitOk => Ok(()),
            other => Err(unexpected_response("commit", &other)),
        }
    }

    pub async fn rollback(&self, lease_key: impl Into<String>) -> Result<(), ActorUdsError> {
        match self
            .request(
                wire::RequestPayload::SqliteRollback(wire::SqliteRollback {
                    lease_key: lease_key.into(),
                }),
                None,
            )
            .await?
        {
            wire::ResponsePayload::SqliteRollbackOk => Ok(()),
            other => Err(unexpected_response("rollback", &other)),
        }
    }

    async fn request(
        &self,
        payload: wire::RequestPayload,
        lease_key: Option<&str>,
    ) -> Result<wire::ResponsePayload, ActorUdsError> {
        let timeout_duration = self.inner.request_timeout;
        match timeout(timeout_duration, self.request_inner(payload, lease_key)).await {
            Ok(result) => result,
            Err(_) => {
                // The timed-out future may have written a request whose response
                // will arrive later. Never reuse that desynchronized stream.
                *self.inner.connection.lock().await = None;
                Err(ActorUdsError::Timeout {
                    operation: "request",
                    timeout_ms: timeout_duration.as_millis().min(u128::from(u64::MAX)) as u64,
                })
            }
        }
    }

    async fn request_inner(
        &self,
        payload: wire::RequestPayload,
        lease_key: Option<&str>,
    ) -> Result<wire::ResponsePayload, ActorUdsError> {
        let mut slot = self.inner.connection.lock().await;
        if slot.is_none() {
            *slot = Some(connect(&self.inner.path).await?);
        }
        let connection = slot.as_mut().expect("connection initialized");
        let request_id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        let frame = wire::ClientFrame::Request(wire::Request {
            request_id,
            lease_key: lease_key.map(str::to_owned),
            payload,
        });
        let encoded = versioned::ClientFrame::wrap_latest(frame)
            .serialize_with_embedded_version(PROTOCOL_VERSION)
            .map_err(|error| ActorUdsError::Protocol(error.to_string()))?;
        if encoded.len() > connection.max_frame_bytes as usize {
            return Err(ActorUdsError::Protocol(format!(
                "request frame is {} bytes, limit is {} bytes",
                encoded.len(),
                connection.max_frame_bytes
            )));
        }
        if let Err(error) = write_frame(&mut connection.stream, &encoded).await {
            *slot = None;
            return Err(error);
        }
        let response = match read_frame(&mut connection.stream, connection.max_frame_bytes).await {
            Ok(response) => response,
            Err(error) => {
                *slot = None;
                return Err(error);
            }
        };
        let frame = match versioned::ServerFrame::deserialize_with_embedded_version(&response) {
            Ok(frame) => frame,
            Err(error) => {
                *slot = None;
                return Err(ActorUdsError::Protocol(error.to_string()));
            }
        };
        match frame {
            wire::ServerFrame::Response(response) if response.request_id == request_id => {
                map_response(response.payload)
            }
            wire::ServerFrame::Response(response) => {
                *slot = None;
                Err(ActorUdsError::Protocol(format!(
                    "response id {} did not match request id {request_id}",
                    response.request_id
                )))
            }
            wire::ServerFrame::GoAway(_) => {
                *slot = None;
                Err(ActorUdsError::Protocol("server sent GoAway".to_owned()))
            }
        }
    }
}

async fn connect(path: &Path) -> Result<Connection, ActorUdsError> {
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
        wire::ServerHello::HelloOk(ok) => Ok(Connection {
            stream,
            max_frame_bytes: ok.max_frame_bytes.min(MAX_FRAME_BYTES),
        }),
        wire::ServerHello::HelloRejectUnsupportedVersion => Err(ActorUdsError::VersionMismatch),
    }
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> Result<(), ActorUdsError> {
    let length = u32::try_from(payload.len())
        .map_err(|_| ActorUdsError::Protocol("frame length exceeds u32".to_owned()))?;
    stream.write_u32(length).await?;
    stream.write_all(payload).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(
    stream: &mut UnixStream,
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
