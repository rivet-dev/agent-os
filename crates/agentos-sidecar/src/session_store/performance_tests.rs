use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use agentos_actor_uds_client::protocol as wire;
use agentos_native_sidecar::limits::{
    DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES, DEFAULT_SQLITE_MAX_RESULT_BYTES,
};
use agentos_native_sidecar::vm_sqlite::{VmSqliteDatabase, VmSqliteError};
use agentos_vm_config::VmSqliteDescriptor;
use async_trait::async_trait;
use rusqlite::types::{Value as SqliteValue, ValueRef};
use rusqlite::{params_from_iter, Connection};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use vbare::OwnedVersionedData;

const HISTORY_COMPLEXITY_LIMIT: usize = 2_048;
const SESSION_LIST_WIRE_BYTE_LIMIT: usize = 8 * 1024 * 1024;
const APPEND_WIRE_BYTE_LIMIT: usize = 64 * 1024;

fn runtime() -> &'static agentos_runtime::SidecarRuntime {
    agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
        .expect("runtime")
}

#[derive(Default)]
struct WireMetrics {
    query_count: AtomicUsize,
    response_bytes: AtomicUsize,
}

impl WireMetrics {
    fn reset(&self) {
        self.query_count.store(0, Ordering::Relaxed);
        self.response_bytes.store(0, Ordering::Relaxed);
    }

    fn snapshot(&self) -> (usize, usize) {
        (
            self.query_count.load(Ordering::Relaxed),
            self.response_bytes.load(Ordering::Relaxed),
        )
    }
}

struct ActorUdsFixture {
    _dir: TempDir,
    path: String,
    connection: Arc<Mutex<Connection>>,
    metrics: Arc<WireMetrics>,
    server: tokio::task::JoinHandle<()>,
}

impl ActorUdsFixture {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("actor.sqlite.sock");
        let listener = UnixListener::bind(&socket).expect("bind actor UDS");
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("open actor SQLite"),
        ));
        let metrics = Arc::new(WireMetrics::default());
        let server_connection = Arc::clone(&connection);
        let server_metrics = Arc::clone(&metrics);
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("accept actor UDS");
                tokio::spawn(serve_actor_connection(
                    stream,
                    Arc::clone(&server_connection),
                    Arc::clone(&server_metrics),
                ));
            }
        });
        Self {
            _dir: dir,
            path: socket.display().to_string(),
            connection,
            metrics,
            server,
        }
    }

    async fn database(&self) -> SharedVmSqliteDatabase {
        agentos_native_sidecar::vm_sqlite::resolve_vm_sqlite(
            &VmSqliteDescriptor::ActorUds {
                path: self.path.clone(),
            },
            runtime().context(),
            DEFAULT_SQLITE_MAX_RESULT_BYTES,
        )
        .await
        .expect("actor database")
    }
}

impl Drop for ActorUdsFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let len = stream.read_u32().await?;
    let mut payload = vec![0; len as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
    stream.write_u32(payload.len() as u32).await?;
    stream.write_all(payload).await?;
    stream.flush().await
}

fn to_sqlite(value: wire::SqlValue) -> SqliteValue {
    match value {
        wire::SqlValue::SqlNull => SqliteValue::Null,
        wire::SqlValue::SqlInteger(value) => SqliteValue::Integer(value),
        wire::SqlValue::SqlReal(value) => SqliteValue::Real(value),
        wire::SqlValue::SqlText(value) => SqliteValue::Text(value),
        wire::SqlValue::SqlBlob(value) => SqliteValue::Blob(value),
    }
}

fn from_sqlite(value: ValueRef<'_>) -> wire::SqlValue {
    match value {
        ValueRef::Null => wire::SqlValue::SqlNull,
        ValueRef::Integer(value) => wire::SqlValue::SqlInteger(value),
        ValueRef::Real(value) => wire::SqlValue::SqlReal(value),
        ValueRef::Text(value) => {
            wire::SqlValue::SqlText(String::from_utf8(value.to_vec()).expect("SQLite text"))
        }
        ValueRef::Blob(value) => wire::SqlValue::SqlBlob(value.to_vec()),
    }
}

fn execute_query(connection: &mut Connection, request: wire::SqliteQuery) -> wire::ResponsePayload {
    let result = (|| -> rusqlite::Result<wire::SqliteQueryOk> {
        let values = request
            .params
            .into_iter()
            .map(to_sqlite)
            .collect::<Vec<_>>();
        let mut statement = connection.prepare(&request.sql)?;
        let columns = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if columns.is_empty() {
            let changes = statement.execute(params_from_iter(values))? as i64;
            return Ok(wire::SqliteQueryOk {
                columns,
                rows: Vec::new(),
                changes,
                last_insert_row_id: Some(connection.last_insert_rowid()),
            });
        }
        let column_count = columns.len();
        let rows = statement
            .query_map(params_from_iter(values), |row| {
                (0..column_count)
                    .map(|index| row.get_ref(index).map(from_sqlite))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(wire::SqliteQueryOk {
            columns,
            rows,
            changes: 0,
            last_insert_row_id: None,
        })
    })();
    match result {
        Ok(result) => wire::ResponsePayload::SqliteQueryOk(result),
        Err(error) => wire::ResponsePayload::SqlError(wire::SqlError {
            code: 1,
            statement_index: 0,
            message: error.to_string(),
        }),
    }
}

async fn serve_actor_connection(
    mut stream: UnixStream,
    connection: Arc<Mutex<Connection>>,
    metrics: Arc<WireMetrics>,
) {
    let Ok(hello_frame) = read_frame(&mut stream).await else {
        return;
    };
    wire::versioned::ClientHello::deserialize_with_embedded_version(&hello_frame)
        .expect("actor client hello");
    let response =
        wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
            max_frame_bytes: 32 * 1024 * 1024,
        }))
        .serialize_with_embedded_version(1)
        .expect("serialize actor hello");
    if write_frame(&mut stream, &response).await.is_err() {
        return;
    }

    while let Ok(payload) = read_frame(&mut stream).await {
        let wire::ClientFrame::Request(request) =
            wire::versioned::ClientFrame::deserialize_with_embedded_version(&payload)
                .expect("actor request");
        let response_payload = {
            let mut connection = connection.lock().expect("actor SQLite mutex");
            match request.payload {
                wire::RequestPayload::SqliteExec(exec) => {
                    match connection.execute_batch(&exec.script) {
                        Ok(()) => wire::ResponsePayload::SqliteExecOk,
                        Err(error) => wire::ResponsePayload::SqlError(wire::SqlError {
                            code: 1,
                            statement_index: 0,
                            message: error.to_string(),
                        }),
                    }
                }
                wire::RequestPayload::SqliteQuery(query) => {
                    metrics.query_count.fetch_add(1, Ordering::Relaxed);
                    execute_query(&mut connection, query)
                }
                wire::RequestPayload::SqliteBegin(_) => {
                    connection
                        .execute_batch("BEGIN IMMEDIATE")
                        .expect("begin actor transaction");
                    wire::ResponsePayload::SqliteBeginOk
                }
                wire::RequestPayload::SqliteCommit(_) => {
                    connection
                        .execute_batch("COMMIT")
                        .expect("commit actor transaction");
                    wire::ResponsePayload::SqliteCommitOk
                }
                wire::RequestPayload::SqliteRollback(_) => {
                    connection
                        .execute_batch("ROLLBACK")
                        .expect("rollback actor transaction");
                    wire::ResponsePayload::SqliteRollbackOk
                }
            }
        };
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: response_payload,
            },
        ))
        .serialize_with_embedded_version(1)
        .expect("serialize actor response");
        metrics
            .response_bytes
            .fetch_add(response.len() + size_of::<u32>(), Ordering::Relaxed);
        if write_frame(&mut stream, &response).await.is_err() {
            return;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecordedCall {
    Query(String),
    Transaction(Vec<String>),
}

struct RecordingDatabase {
    inner: SharedVmSqliteDatabase,
    calls: Mutex<Vec<RecordedCall>>,
}

impl RecordingDatabase {
    fn wrap(inner: SharedVmSqliteDatabase) -> Arc<Self> {
        Arc::new(Self {
            inner,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("recording mutex").clone()
    }
}

#[async_trait]
impl VmSqliteDatabase for RecordingDatabase {
    async fn query(&self, statement: SqlStatement) -> Result<QueryResult, VmSqliteError> {
        self.calls
            .lock()
            .expect("recording mutex")
            .push(RecordedCall::Query(statement.sql.clone()));
        self.inner.query(statement).await
    }

    async fn transaction(
        &self,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<QueryResult>, VmSqliteError> {
        self.calls
            .lock()
            .expect("recording mutex")
            .push(RecordedCall::Transaction(
                statements
                    .iter()
                    .map(|statement| statement.sql.clone())
                    .collect(),
            ));
        self.inner.transaction(statements).await
    }
}

fn populate_sessions(connection: &mut Connection, count: usize) {
    connection
        .execute(
            "WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < ?1) \
             INSERT INTO agentos_core_sessions (session_id, agent, acp_session_id, state, cwd, permission_policy, skip_os_instructions, additional_directories_json, env_json, mcp_servers_json, config_options_json, created_at_ms, updated_at_ms) \
             SELECT printf('session-%05d', n), 'agent', printf('private-%05d', n), 'idle', '/workspace', 'allow_all', 0, '[]', '{\"PRIVATE_TOKEN\":\"not-listed\"}', '[]', '[]', n, n FROM seq",
            [i64::try_from(count).expect("session count")],
        )
        .expect("populate sessions");
}

async fn populate_history(database: &SharedVmSqliteDatabase, count: usize) {
    let payload =
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"retained"}}"#;
    database
        .transaction(vec![
            SqlStatement::new(
                "WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < ?) INSERT INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, payload_json, payload_bytes) SELECT 'main', n, n, 1, 'session_update', ?, length(CAST(? AS BLOB)) FROM seq",
                vec![
                    SqlValue::SqlInteger(i64::try_from(count).expect("history count")),
                    text(payload),
                    text(payload),
                ],
            ),
            SqlStatement::new(
                "UPDATE agentos_core_sessions SET latest_sequence = ?, retained_event_count = ?, retained_event_bytes = (SELECT SUM(payload_bytes) FROM agentos_core_events WHERE session_id = 'main') WHERE session_id = 'main'",
                vec![
                    SqlValue::SqlInteger(i64::try_from(count).expect("history count")),
                    SqlValue::SqlInteger(i64::try_from(count).expect("history count")),
                ],
            ),
        ])
        .await
        .expect("populate history");
}

fn assert_constant_append_shape(calls: &[RecordedCall]) {
    assert_eq!(
        calls.len(),
        2,
        "append must use one transaction and one retention read"
    );
    let RecordedCall::Transaction(statements) = &calls[0] else {
        panic!("append did not start with its transaction: {calls:?}");
    };
    assert_eq!(
        statements.len(),
        4,
        "single-event append transaction changed shape"
    );
    assert!(matches!(&calls[1], RecordedCall::Query(sql) if sql.contains("retained_event_count")));
    for sql in statements
        .iter()
        .chain(calls.iter().filter_map(|call| match call {
            RecordedCall::Query(sql) => Some(sql),
            RecordedCall::Transaction(_) => None,
        }))
    {
        let normalized = sql.to_ascii_uppercase();
        assert!(
            !normalized.contains("COUNT("),
            "append rescans event count: {sql}"
        );
        assert!(
            !normalized.contains("SUM("),
            "append rescans event bytes: {sql}"
        );
        assert!(
            !normalized.contains(" OVER "),
            "append uses a history window scan: {sql}"
        );
    }
}

#[test]
fn maximum_session_page_is_two_queries_and_bounded_over_actor_uds() {
    runtime().block_on(async {
        let fixture = ActorUdsFixture::start().await;
        let database = fixture.database().await;
        let store = SessionStore::open(database).await.expect("session store");
        populate_sessions(
            &mut fixture.connection.lock().expect("actor SQLite mutex"),
            DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES,
        );

        fixture.metrics.reset();
        let sessions = store
            .list(None, DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES)
            .await
            .expect("maximum session page");
        let (query_count, response_bytes) = fixture.metrics.snapshot();

        assert_eq!(sessions.len(), DEFAULT_ACP_MAX_SESSION_LIST_ENTRIES);
        assert!(
            (1..=2).contains(&query_count),
            "session listing used {query_count} queries; expected one joined query or two bounded queries"
        );
        assert!(
            response_bytes <= SESSION_LIST_WIRE_BYTE_LIMIT,
            "maximum session page used {response_bytes} actor UDS response bytes; bound is {SESSION_LIST_WIRE_BYTE_LIMIT}"
        );
    });
}

#[test]
fn append_work_is_constant_near_history_limit_on_local_file_and_actor_uds() {
    runtime().block_on(async {
        let dir = tempfile::tempdir().expect("tempdir");
        let local = agentos_native_sidecar::vm_sqlite::resolve_vm_sqlite(
            &VmSqliteDescriptor::SqliteFile {
                path: dir.path().join("history.sqlite").display().to_string(),
            },
            runtime().context(),
            DEFAULT_SQLITE_MAX_RESULT_BYTES,
        )
        .await
        .expect("local database");
        exercise_near_limit_append("local-file", local, None).await;

        let actor = ActorUdsFixture::start().await;
        let actor_database = actor.database().await;
        exercise_near_limit_append("actor-uds", actor_database, Some(&actor.metrics)).await;
    });
}

async fn exercise_near_limit_append(
    backend: &str,
    database: SharedVmSqliteDatabase,
    wire_metrics: Option<&WireMetrics>,
) {
    let store = SessionStore::open(Arc::clone(&database))
        .await
        .expect("session store");
    store
        .create(
            "main",
            "agent",
            "native",
            "/workspace",
            "{}",
            None,
            None,
            "[]",
        )
        .await
        .expect("create session");
    populate_history(&database, HISTORY_COMPLEXITY_LIMIT - 1).await;

    let recording = RecordingDatabase::wrap(database);
    let limits = AcpLimits {
        max_session_history_events: HISTORY_COMPLEXITY_LIMIT,
        max_session_history_bytes: 16 * 1024 * 1024,
        ..AcpLimits::default()
    };
    let store: SharedVmSqliteDatabase = recording.clone();
    let store = SessionStore::from_database(store).with_limits(&limits);
    if let Some(metrics) = wire_metrics {
        metrics.reset();
    }

    let started = Instant::now();
    let appended = store
        .append_updates(
            "main",
            1,
            &[serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "at the limit"}
            })],
        )
        .await
        .expect("near-limit append");
    let elapsed = started.elapsed();
    eprintln!(
        "near-limit durable history append: backend={backend} retained_events={} elapsed={elapsed:?}",
        HISTORY_COMPLEXITY_LIMIT - 1
    );
    assert_eq!(appended.len(), 1);
    assert_constant_append_shape(&recording.calls());

    if let Some(metrics) = wire_metrics {
        let (query_count, response_bytes) = metrics.snapshot();
        assert_eq!(
            query_count, 5,
            "actor append must be four statements plus one retention read; transaction control uses dedicated frames"
        );
        assert!(
            response_bytes <= APPEND_WIRE_BYTE_LIMIT,
            "single near-limit append used {response_bytes} actor UDS response bytes; bound is {APPEND_WIRE_BYTE_LIMIT}"
        );
    }
}
