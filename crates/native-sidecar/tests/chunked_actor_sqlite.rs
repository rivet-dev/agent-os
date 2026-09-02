use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentos_actor_uds_client::protocol as wire;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection};
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Notify;
use tokio::time::timeout;
use vbare::OwnedVersionedData;

// The included plugin only needs the mount context's runtime handle. Keep the
// integration test independent from the full native-sidecar service graph.
mod bridge {
    pub struct MountPluginContext<B> {
        pub runtime_context: agentos_runtime::RuntimeContext,
        pub database: Option<crate::vm_sqlite::SharedVmSqliteDatabase>,
        pub marker: std::marker::PhantomData<B>,
    }
}

#[allow(dead_code)]
#[path = "../src/vm_sqlite.rs"]
mod vm_sqlite;

#[allow(dead_code)]
mod subject {
    include!("../src/plugins/chunked_actor_sqlite.rs");

    pub async fn exercise_persistence(client: SharedVmSqliteDatabase) {
        let first = ActorSqliteMetadataStore::new(
            client.clone(),
            "test".to_owned(),
            DEFAULT_MAX_METADATA_BYTES,
        );
        let root = first.resolve("/").await.unwrap();
        first
            .create(
                root.ino,
                "workspace",
                CreateInodeAttrs::directory(0o755, 1000, 1000),
            )
            .await
            .unwrap();

        let reopened = ActorSqliteMetadataStore::new(
            client.clone(),
            "test".to_owned(),
            DEFAULT_MAX_METADATA_BYTES,
        );
        let workspace = reopened.resolve("/workspace").await.unwrap();
        assert_eq!(workspace.uid, 1000);

        let blocks = ActorSqliteBlockStore::new(client.clone(), "test".to_owned());
        let first_key = BlockKey("first".to_owned());
        let copied_key = BlockKey("copied".to_owned());
        blocks.put(&first_key, b"persisted bytes").await.unwrap();
        assert_eq!(blocks.get(&first_key).await.unwrap(), b"persisted bytes");
        blocks.copy(&first_key, &copied_key).await.unwrap();
        assert_eq!(blocks.get(&copied_key).await.unwrap(), b"persisted bytes");
        blocks
            .delete_many(&[first_key.clone(), copied_key.clone()])
            .await
            .unwrap();
        assert!(!blocks.exists(&first_key).await.unwrap());
        assert!(!blocks.exists(&copied_key).await.unwrap());

        let large_metadata = InMemoryMetadataStore::new();
        let root = large_metadata.resolve("/").await.unwrap();
        large_metadata
            .create(
                root.ino,
                "large-inline",
                CreateInodeAttrs::file(
                    0o644,
                    0,
                    0,
                    vfs::engine::types::Storage::Inline(vec![7; METADATA_CHUNK_SIZE * 2 + 17]),
                ),
            )
            .await
            .unwrap();
        persist_metadata(
            &client,
            "large-test",
            &large_metadata,
            DEFAULT_MAX_METADATA_BYTES,
        )
        .await
        .unwrap();
        let chunks = client
            .query(SqlStatement::new(
                "SELECT content FROM agentos_fs_metadata_chunks WHERE namespace = ? ORDER BY chunk_index",
                vec![SqlValue::SqlText("large-test".to_owned())],
            ))
            .await
            .unwrap();
        assert!(chunks.rows.len() >= 3);
        assert!(chunks.rows.iter().all(|row| {
            matches!(row.first(), Some(SqlValue::SqlBlob(bytes)) if bytes.len() <= METADATA_CHUNK_SIZE)
        }));
        let loaded = load_metadata(&client, "large-test", DEFAULT_MAX_METADATA_BYTES)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, serde_bare::to_vec(&large_metadata.dump()).unwrap());
    }
}

async fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let len = stream.read_u32().await.unwrap();
    let mut payload = vec![0; len as usize];
    stream.read_exact(&mut payload).await.unwrap();
    payload
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) {
    stream.write_u32(payload.len() as u32).await.unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

fn to_sqlite(value: wire::SqlValue) -> Value {
    match value {
        wire::SqlValue::SqlNull => Value::Null,
        wire::SqlValue::SqlInteger(value) => Value::Integer(value),
        wire::SqlValue::SqlReal(value) => Value::Real(value),
        wire::SqlValue::SqlText(value) => Value::Text(value),
        wire::SqlValue::SqlBlob(value) => Value::Blob(value),
    }
}

fn from_sqlite(value: ValueRef<'_>) -> wire::SqlValue {
    match value {
        ValueRef::Null => wire::SqlValue::SqlNull,
        ValueRef::Integer(value) => wire::SqlValue::SqlInteger(value),
        ValueRef::Real(value) => wire::SqlValue::SqlReal(value),
        ValueRef::Text(value) => {
            wire::SqlValue::SqlText(String::from_utf8(value.to_vec()).unwrap())
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

async fn serve_connection(mut stream: UnixStream, database: Arc<Mutex<Connection>>) {
    wire::versioned::ClientHello::deserialize_with_embedded_version(&read_frame(&mut stream).await)
        .unwrap();
    let response =
        wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
            max_frame_bytes: 32 * 1024 * 1024,
        }))
        .serialize_with_embedded_version(1)
        .unwrap();
    write_frame(&mut stream, &response).await;

    loop {
        let payload = match stream.read_u32().await {
            Ok(len) => {
                let mut payload = vec![0; len as usize];
                stream.read_exact(&mut payload).await.unwrap();
                payload
            }
            Err(_) => return,
        };
        let wire::ClientFrame::Request(request) =
            wire::versioned::ClientFrame::deserialize_with_embedded_version(&payload).unwrap();
        let response = {
            let mut connection = database.lock().unwrap();
            match request.payload {
                wire::RequestPayload::SqliteExec(exec) => {
                    connection.execute_batch(&exec.script).unwrap();
                    wire::ResponsePayload::SqliteExecOk
                }
                wire::RequestPayload::SqliteQuery(query) => execute_query(&mut connection, query),
                wire::RequestPayload::SqliteBegin(_) => {
                    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
                    wire::ResponsePayload::SqliteBeginOk
                }
                wire::RequestPayload::SqliteCommit(_) => {
                    connection.execute_batch("COMMIT").unwrap();
                    wire::ResponsePayload::SqliteCommitOk
                }
                wire::RequestPayload::SqliteRollback(_) => {
                    connection.execute_batch("ROLLBACK").unwrap();
                    wire::ResponsePayload::SqliteRollbackOk
                }
            }
        };
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id: request.request_id,
                payload: response,
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await;
    }
}

struct TransactionRaceGate {
    first_leased_query: Notify,
    unleased_query_received: Notify,
    release_first_leased_query: Notify,
}

async fn serve_transaction_race_connection(mut stream: UnixStream, gate: Arc<TransactionRaceGate>) {
    wire::versioned::ClientHello::deserialize_with_embedded_version(&read_frame(&mut stream).await)
        .unwrap();
    let response =
        wire::versioned::ServerHello::wrap_latest(wire::ServerHello::HelloOk(wire::HelloOk {
            max_frame_bytes: 32 * 1024 * 1024,
        }))
        .serialize_with_embedded_version(1)
        .unwrap();
    write_frame(&mut stream, &response).await;

    let mut connection = Connection::open_in_memory().unwrap();
    let mut active_lease = None;
    let mut leased_query_count = 0;
    let mut parked_unleased_query = None;
    loop {
        let payload = match stream.read_u32().await {
            Ok(len) => {
                let mut payload = vec![0; len as usize];
                stream.read_exact(&mut payload).await.unwrap();
                payload
            }
            Err(_) => return,
        };
        let wire::ClientFrame::Request(request) =
            wire::versioned::ClientFrame::deserialize_with_embedded_version(&payload).unwrap();
        let request_id = request.request_id;
        let response = match request.payload {
            wire::RequestPayload::SqliteExec(exec) => {
                connection.execute_batch(&exec.script).unwrap();
                wire::ResponsePayload::SqliteExecOk
            }
            wire::RequestPayload::SqliteQuery(query) => {
                match request.lease_key.as_deref() {
                    Some(lease_key) => {
                        assert_eq!(active_lease.as_deref(), Some(lease_key));
                        leased_query_count += 1;
                        let response = execute_query(&mut connection, query);
                        if leased_query_count == 1 {
                            gate.first_leased_query.notify_one();
                            let wire::ClientFrame::Request(parked) =
                                wire::versioned::ClientFrame::deserialize_with_embedded_version(
                                    &read_frame(&mut stream).await,
                                )
                                .unwrap();
                            assert!(parked.lease_key.is_none());
                            let wire::RequestPayload::SqliteQuery(query) = parked.payload else {
                                panic!("expected the standalone query while the transaction was active");
                            };
                            parked_unleased_query = Some((parked.request_id, query));
                            gate.unleased_query_received.notify_one();
                            gate.release_first_leased_query.notified().await;
                        }
                        response
                    }
                    None if active_lease.is_some() => panic!(
                    "the standalone query should be parked while the transaction lease is active"
                ),
                    None => execute_query(&mut connection, query),
                }
            }
            wire::RequestPayload::SqliteBegin(begin) => {
                assert!(active_lease.replace(begin.lease_key).is_none());
                connection.execute_batch("BEGIN IMMEDIATE").unwrap();
                wire::ResponsePayload::SqliteBeginOk
            }
            wire::RequestPayload::SqliteCommit(commit) => {
                assert_eq!(active_lease.as_deref(), Some(commit.lease_key.as_str()));
                active_lease = None;
                connection.execute_batch("COMMIT").unwrap();
                wire::ResponsePayload::SqliteCommitOk
            }
            wire::RequestPayload::SqliteRollback(rollback) => {
                assert_eq!(active_lease.as_deref(), Some(rollback.lease_key.as_str()));
                active_lease = None;
                connection.execute_batch("ROLLBACK").unwrap();
                wire::ResponsePayload::SqliteRollbackOk
            }
        };
        let response = wire::versioned::ServerFrame::wrap_latest(wire::ServerFrame::Response(
            wire::Response {
                request_id,
                payload: response,
            },
        ))
        .serialize_with_embedded_version(1)
        .unwrap();
        write_frame(&mut stream, &response).await;

        if active_lease.is_none() {
            if let Some((request_id, query)) = parked_unleased_query.take() {
                let payload = execute_query(&mut connection, query);
                let response = wire::versioned::ServerFrame::wrap_latest(
                    wire::ServerFrame::Response(wire::Response {
                        request_id,
                        payload,
                    }),
                )
                .serialize_with_embedded_version(1)
                .unwrap();
                write_frame(&mut stream, &response).await;
            }
        }
    }
}

#[tokio::test]
async fn metadata_and_blocks_persist_directly_over_actor_sqlite_uds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let database = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let database = database.clone();
            tokio::spawn(serve_connection(stream, database));
        }
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let client = vm_sqlite::resolve_vm_sqlite(
        &agentos_vm_config::VmSqliteDescriptor::ActorUds {
            path: path.display().to_string(),
        },
        runtime.context(),
        agentos_native_sidecar_core::limits::SqliteLimits::default(),
    )
    .await
    .unwrap();
    subject::bootstrap_schema(client.as_ref()).await.unwrap();
    subject::exercise_persistence(client).await;
    server.abort();
}

#[tokio::test]
async fn migrations_are_independent_strict_and_atomic_over_actor_sqlite_uds() {
    const FS_MIGRATIONS: &[vm_sqlite::VmSqliteMigration] = &[vm_sqlite::VmSqliteMigration {
        version: 1,
        statements: &["CREATE TABLE agentos_fs_uds_probe (value INTEGER NOT NULL) STRICT"],
    }];
    const CORE_MIGRATIONS: &[vm_sqlite::VmSqliteMigration] = &[vm_sqlite::VmSqliteMigration {
        version: 1,
        statements: &["CREATE TABLE agentos_core_uds_probe (value TEXT NOT NULL) STRICT"],
    }];
    const FAILING_ACTOR_MIGRATIONS: &[vm_sqlite::VmSqliteMigration] =
        &[vm_sqlite::VmSqliteMigration {
            version: 1,
            statements: &[
                "CREATE TABLE agentos_actor_uds_probe (value INTEGER NOT NULL) STRICT",
                "INSERT INTO agentos_actor_uds_probe (value) VALUES ('not-an-integer')",
            ],
        }];

    let dir = tempdir().unwrap();
    let path = dir.path().join("actor-migrations.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let database = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let database = database.clone();
            tokio::spawn(serve_connection(stream, database));
        }
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let client = vm_sqlite::resolve_vm_sqlite(
        &agentos_vm_config::VmSqliteDescriptor::ActorUds {
            path: path.display().to_string(),
        },
        runtime.context(),
        agentos_native_sidecar_core::limits::SqliteLimits::default(),
    )
    .await
    .unwrap();

    vm_sqlite::migrate_schema(
        client.as_ref(),
        "filesystem",
        "agentos_fs_schema_version",
        FS_MIGRATIONS,
    )
    .await
    .unwrap();
    vm_sqlite::migrate_schema(
        client.as_ref(),
        "core",
        "agentos_core_schema_version",
        CORE_MIGRATIONS,
    )
    .await
    .unwrap();
    let versions = client
        .query(vm_sqlite::SqlStatement::plain(
            "SELECT (SELECT schema_version FROM agentos_fs_schema_version WHERE singleton = 1), (SELECT schema_version FROM agentos_core_schema_version WHERE singleton = 1), (SELECT COUNT(*) FROM sqlite_schema WHERE name = 'agentos_schema_versions')",
        ))
        .await
        .unwrap();
    assert_eq!(
        versions.rows,
        vec![vec![
            vm_sqlite::SqlValue::SqlInteger(1),
            vm_sqlite::SqlValue::SqlInteger(1),
            vm_sqlite::SqlValue::SqlInteger(0),
        ]]
    );

    let schemas = client
        .query(vm_sqlite::SqlStatement::plain(
            "SELECT name, sql FROM sqlite_schema WHERE name IN ('agentos_fs_schema_version', 'agentos_fs_uds_probe', 'agentos_core_schema_version', 'agentos_core_uds_probe') ORDER BY name",
        ))
        .await
        .unwrap();
    assert_eq!(schemas.rows.len(), 4);
    assert!(schemas.rows.iter().all(|row| {
        matches!(row.get(1), Some(vm_sqlite::SqlValue::SqlText(sql)) if sql.trim_end().ends_with("STRICT"))
    }));

    let invalid_type = client
        .query(vm_sqlite::SqlStatement::plain(
            "INSERT INTO agentos_fs_uds_probe (value) VALUES ('text')",
        ))
        .await;
    assert!(matches!(
        invalid_type,
        Err(vm_sqlite::VmSqliteError::Actor(
            agentos_actor_uds_client::ActorUdsError::Sql { .. }
        ))
    ));

    assert!(vm_sqlite::migrate_schema(
        client.as_ref(),
        "actor",
        "agentos_actor_schema_version",
        FAILING_ACTOR_MIGRATIONS,
    )
    .await
    .is_err());
    let rolled_back = client
        .query(vm_sqlite::SqlStatement::plain(
            "SELECT (SELECT COUNT(*) FROM sqlite_schema WHERE name = 'agentos_actor_uds_probe'), (SELECT COUNT(*) FROM agentos_actor_schema_version), (SELECT schema_version FROM agentos_fs_schema_version WHERE singleton = 1), (SELECT schema_version FROM agentos_core_schema_version WHERE singleton = 1)",
        ))
        .await
        .unwrap();
    assert_eq!(
        rolled_back.rows,
        vec![vec![
            vm_sqlite::SqlValue::SqlInteger(0),
            vm_sqlite::SqlValue::SqlInteger(0),
            vm_sqlite::SqlValue::SqlInteger(1),
            vm_sqlite::SqlValue::SqlInteger(1),
        ]]
    );
    server.abort();
}

#[tokio::test]
async fn standalone_queries_do_not_deadlock_actor_uds_transactions() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("actor-transaction-race.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let gate = Arc::new(TransactionRaceGate {
        first_leased_query: Notify::new(),
        unleased_query_received: Notify::new(),
        release_first_leased_query: Notify::new(),
    });
    let server_gate = Arc::clone(&gate);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        serve_transaction_race_connection(stream, server_gate).await;
    });

    let runtime =
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .unwrap();
    let database = vm_sqlite::resolve_vm_sqlite(
        &agentos_vm_config::VmSqliteDescriptor::ActorUds {
            path: path.display().to_string(),
        },
        runtime.context(),
        agentos_native_sidecar_core::limits::SqliteLimits::default(),
    )
    .await
    .unwrap();

    let transaction_database = Arc::clone(&database);
    let transaction = tokio::spawn(async move {
        transaction_database
            .transaction(vec![
                vm_sqlite::SqlStatement::plain("SELECT 1"),
                vm_sqlite::SqlStatement::plain("SELECT 2"),
            ])
            .await
    });
    timeout(Duration::from_secs(1), gate.first_leased_query.notified())
        .await
        .expect("transaction did not reach its first query");

    let query_database = Arc::clone(&database);
    let query = tokio::spawn(async move {
        query_database
            .query(vm_sqlite::SqlStatement::plain("SELECT 3"))
            .await
    });
    timeout(
        Duration::from_secs(1),
        gate.unleased_query_received.notified(),
    )
    .await
    .expect("standalone query did not reach the server before the transaction response");
    gate.release_first_leased_query.notify_one();

    let (transaction, query) = timeout(Duration::from_secs(1), async {
        tokio::join!(transaction, query)
    })
    .await
    .expect("standalone query deadlocked the active actor UDS transaction");
    assert_eq!(transaction.unwrap().unwrap().len(), 2);
    assert_eq!(query.unwrap().unwrap().rows.len(), 1);
    server.abort();
}
