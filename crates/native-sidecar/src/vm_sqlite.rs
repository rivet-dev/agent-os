//! VM-scoped SQLite substrate shared by VFS and AgentOS durable state.
//!
//! The actor backend is intentionally a thin translation over `ActorUdsClient`.
//! Rivet owns transaction isolation through lease keys, so every transaction
//! uses one fresh UUID and attaches it to `BEGIN`, every statement, and the
//! terminal `COMMIT` or `ROLLBACK`. No second mux, pool, or retry scheduler is
//! needed in the sidecar.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentos_actor_uds_client::{ActorUdsClient, ActorUdsError};
pub use agentos_actor_uds_client::{QueryResult, SqlValue};
use async_trait::async_trait;
use rusqlite::types::{Value, ValueRef};
use thiserror::Error;
use uuid::Uuid;

use agentos_vm_config::VmSqliteDescriptor;

const LOCAL_SQLITE_JOB_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct SqlStatement {
    pub sql: String,
    pub params: Vec<SqlValue>,
    expected_changes: Option<i64>,
}

impl SqlStatement {
    pub fn new(sql: impl Into<String>, params: Vec<SqlValue>) -> Self {
        Self {
            sql: sql.into(),
            params,
            expected_changes: None,
        }
    }

    pub fn plain(sql: impl Into<String>) -> Self {
        Self::new(sql, Vec::new())
    }

    /// Require this statement to affect exactly `expected` rows. Transaction
    /// backends evaluate this before COMMIT so a failed compare-and-set rolls
    /// the complete operation back.
    pub fn expect_changes(mut self, expected: i64) -> Self {
        self.expected_changes = Some(expected);
        self
    }
}

#[derive(Debug, Error)]
pub enum VmSqliteError {
    #[error("actor SQLite UDS failed: {0}")]
    Actor(#[from] ActorUdsError),
    #[error("local SQLite failed: {0}")]
    Local(#[from] rusqlite::Error),
    #[error("local SQLite blocking executor failed: {0}")]
    Blocking(#[from] agentos_runtime::BlockingJobError),
    #[error("invalid SQLite result: {0}")]
    InvalidResult(String),
    #[error(
        "sqlite_result_limit: SQLite result used {used} bytes, limit {limit}; raise limits.sqlite.maxResultBytes"
    )]
    ResultTooLarge { used: usize, limit: usize },
    #[error(
        "acp_history_events_limit: durable event batch contains {used} events, retention limit {limit}; raise limits.acp.maxSessionHistoryEvents"
    )]
    HistoryEventBatchTooLarge { used: usize, limit: usize },
    #[error(
        "acp_history_bytes_limit: durable event batch uses {used} bytes, retention limit {limit}; raise limits.acp.maxSessionHistoryBytes"
    )]
    HistoryByteBatchTooLarge { used: usize, limit: usize },
    #[error("{code}: durable SQLite collection used {used} rows, limit {limit}; raise {setting}")]
    DurableCollectionLimit {
        code: &'static str,
        used: usize,
        limit: usize,
        setting: &'static str,
    },
    #[error("SQLite backend {backend} did not enable PRAGMA foreign_keys")]
    ForeignKeysDisabled { backend: &'static str },
    #[error("SQLite compare-and-set affected {actual} rows; expected {expected}")]
    UnexpectedChanges { expected: i64, actual: i64 },
    #[error(
        "schema component {component} is at future version {found}; sidecar supports {supported}"
    )]
    FutureSchema {
        component: String,
        found: i64,
        supported: i64,
    },
    #[error("schema migration ladder for {component} skipped version {expected}")]
    InvalidMigrationLadder { component: String, expected: i64 },
}

#[async_trait]
pub trait VmSqliteDatabase: Send + Sync {
    async fn query(&self, statement: SqlStatement) -> Result<QueryResult, VmSqliteError>;

    /// Execute all statements atomically and return their results in order.
    async fn transaction(
        &self,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<QueryResult>, VmSqliteError>;
}

pub type SharedVmSqliteDatabase = Arc<dyn VmSqliteDatabase>;

pub async fn resolve_vm_sqlite(
    descriptor: &VmSqliteDescriptor,
    runtime: agentos_runtime::RuntimeContext,
    max_result_bytes: usize,
) -> Result<SharedVmSqliteDatabase, VmSqliteError> {
    match descriptor {
        VmSqliteDescriptor::ActorUds { path } => Ok(Arc::new(
            ActorUdsVmSqliteDatabase::open(path.clone(), max_result_bytes).await?,
        )),
        VmSqliteDescriptor::SqliteFile { path } => Ok(Arc::new(
            LocalVmSqliteDatabase::open(PathBuf::from(path), runtime, max_result_bytes).await?,
        )),
    }
}

#[derive(Clone)]
struct ActorUdsVmSqliteDatabase {
    client: ActorUdsClient,
    max_result_bytes: usize,
}

impl ActorUdsVmSqliteDatabase {
    async fn open(path: String, max_result_bytes: usize) -> Result<Self, VmSqliteError> {
        let database = Self {
            client: ActorUdsClient::new(path),
            max_result_bytes,
        };
        database.enable_and_verify_foreign_keys().await?;
        Ok(database)
    }

    async fn enable_and_verify_foreign_keys(&self) -> Result<(), VmSqliteError> {
        self.client
            .query("PRAGMA foreign_keys = ON", Vec::new())
            .await?;
        let result = self.client.query("PRAGMA foreign_keys", Vec::new()).await?;
        verify_foreign_keys(&result, "actor_uds")
    }
}

#[async_trait]
impl VmSqliteDatabase for ActorUdsVmSqliteDatabase {
    async fn query(&self, statement: SqlStatement) -> Result<QueryResult, VmSqliteError> {
        let result = self.client.query(statement.sql, statement.params).await?;
        validate_result_size(&result, self.max_result_bytes)?;
        Ok(result)
    }

    async fn transaction(
        &self,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<QueryResult>, VmSqliteError> {
        let key = Uuid::new_v4().to_string();
        self.client.begin(&key, None).await?;
        let mut results = Vec::with_capacity(statements.len());
        for statement in statements {
            let expected_changes = statement.expected_changes;
            match self
                .client
                .query_with_lease(statement.sql, statement.params, Some(&key))
                .await
            {
                Ok(result) => {
                    if let Err(error) = validate_expected_changes(expected_changes, &result) {
                        if let Err(rollback_error) = self.client.rollback(&key).await {
                            eprintln!(
                                "ERR_AGENTOS_SQLITE_ROLLBACK: actor transaction {key} rollback failed after {error}: {rollback_error}"
                            );
                        }
                        return Err(error);
                    }
                    if let Err(error) = validate_result_size(&result, self.max_result_bytes) {
                        if let Err(rollback_error) = self.client.rollback(&key).await {
                            eprintln!(
                                "ERR_AGENTOS_SQLITE_ROLLBACK: actor transaction {key} rollback failed after {error}: {rollback_error}"
                            );
                        }
                        return Err(error);
                    }
                    results.push(result)
                }
                Err(error) => {
                    if let Err(rollback_error) = self.client.rollback(&key).await {
                        eprintln!(
                            "ERR_AGENTOS_SQLITE_ROLLBACK: actor transaction {key} rollback failed after {error}: {rollback_error}"
                        );
                    }
                    return Err(error.into());
                }
            }
        }
        if let Err(error) = self.client.commit(&key).await {
            if let Err(rollback_error) = self.client.rollback(&key).await {
                eprintln!(
                    "ERR_AGENTOS_SQLITE_ROLLBACK: actor transaction {key} rollback failed after commit error {error}: {rollback_error}"
                );
            }
            return Err(error.into());
        }
        Ok(results)
    }
}

struct LocalVmSqliteDatabase {
    connection: Arc<Mutex<rusqlite::Connection>>,
    runtime: agentos_runtime::RuntimeContext,
    max_result_bytes: usize,
}

impl LocalVmSqliteDatabase {
    async fn open(
        path: PathBuf,
        runtime: agentos_runtime::RuntimeContext,
        max_result_bytes: usize,
    ) -> Result<Self, VmSqliteError> {
        let connection = runtime
            .blocking()
            .run(LOCAL_SQLITE_JOB_BYTES, move || {
                let connection = rusqlite::Connection::open(path)?;
                connection.busy_timeout(Duration::from_secs(5))?;
                connection.execute_batch("PRAGMA foreign_keys = ON;")?;
                let enabled: i64 =
                    connection.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
                if enabled != 1 {
                    return Err(VmSqliteError::ForeignKeysDisabled {
                        backend: "sqlite_file",
                    });
                }
                Ok::<_, VmSqliteError>(connection)
            })
            .await??;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            runtime,
            max_result_bytes,
        })
    }

    async fn run<T>(
        &self,
        operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T, VmSqliteError> + Send + 'static,
    ) -> Result<T, VmSqliteError>
    where
        T: Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        self.runtime
            .blocking()
            .run(LOCAL_SQLITE_JOB_BYTES, move || {
                let mut connection = connection.lock().map_err(|_| {
                    VmSqliteError::InvalidResult("local SQLite mutex poisoned".to_owned())
                })?;
                operation(&mut connection)
            })
            .await?
    }
}

#[async_trait]
impl VmSqliteDatabase for LocalVmSqliteDatabase {
    async fn query(&self, statement: SqlStatement) -> Result<QueryResult, VmSqliteError> {
        let max_result_bytes = self.max_result_bytes;
        self.run(move |connection| {
            execute_local_statement(connection, &statement, max_result_bytes)
        })
        .await
    }

    async fn transaction(
        &self,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<QueryResult>, VmSqliteError> {
        let max_result_bytes = self.max_result_bytes;
        self.run(move |connection| {
            connection.execute_batch("BEGIN IMMEDIATE")?;
            let mut results = Vec::with_capacity(statements.len());
            for statement in &statements {
                match execute_local_statement(connection, statement, max_result_bytes) {
                    Ok(result) => {
                        if let Err(error) =
                            validate_expected_changes(statement.expected_changes, &result)
                        {
                            if let Err(rollback_error) = connection.execute_batch("ROLLBACK") {
                                eprintln!(
                                    "ERR_AGENTOS_SQLITE_ROLLBACK: local transaction rollback failed after {error}: {rollback_error}"
                                );
                            }
                            return Err(error);
                        }
                        results.push(result)
                    }
                    Err(error) => {
                        if let Err(rollback_error) = connection.execute_batch("ROLLBACK") {
                            eprintln!(
                                "ERR_AGENTOS_SQLITE_ROLLBACK: local transaction rollback failed after {error}: {rollback_error}"
                            );
                        }
                        return Err(error);
                    }
                }
            }
            if let Err(error) = connection.execute_batch("COMMIT") {
                if let Err(rollback_error) = connection.execute_batch("ROLLBACK") {
                    eprintln!(
                        "ERR_AGENTOS_SQLITE_ROLLBACK: local rollback failed after commit error {error}: {rollback_error}"
                    );
                }
                return Err(error.into());
            }
            Ok(results)
        })
        .await
    }
}

fn validate_expected_changes(
    expected_changes: Option<i64>,
    result: &QueryResult,
) -> Result<(), VmSqliteError> {
    if let Some(expected) = expected_changes {
        if result.changes != expected {
            return Err(VmSqliteError::UnexpectedChanges {
                expected,
                actual: result.changes,
            });
        }
    }
    Ok(())
}

fn execute_local_statement(
    connection: &mut rusqlite::Connection,
    statement: &SqlStatement,
    max_result_bytes: usize,
) -> Result<QueryResult, VmSqliteError> {
    let values = statement
        .params
        .iter()
        .map(sql_value_to_local)
        .collect::<Result<Vec<_>, _>>()?;
    let mut prepared = connection.prepare(&statement.sql)?;
    let columns = prepared
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if columns.is_empty() {
        let changes = prepared.execute(rusqlite::params_from_iter(values.iter()))?;
        return Ok(QueryResult {
            columns,
            rows: Vec::new(),
            changes: i64::try_from(changes).unwrap_or(i64::MAX),
            last_insert_row_id: Some(connection.last_insert_rowid()),
        });
    }

    let column_count = columns.len();
    let mut cursor = prepared.query(rusqlite::params_from_iter(values.iter()))?;
    let mut rows = Vec::new();
    let mut result_bytes = columns.iter().map(String::len).sum::<usize>();
    let mut warned = false;
    while let Some(row) = cursor.next()? {
        let mut output = Vec::with_capacity(column_count);
        for index in 0..column_count {
            let value = local_value_to_sql(row.get_ref(index)?)?;
            result_bytes = result_bytes.checked_add(sql_value_bytes(&value)).ok_or(
                VmSqliteError::ResultTooLarge {
                    used: usize::MAX,
                    limit: max_result_bytes,
                },
            )?;
            if result_bytes > max_result_bytes {
                return Err(VmSqliteError::ResultTooLarge {
                    used: result_bytes,
                    limit: max_result_bytes,
                });
            }
            if !warned && result_bytes >= max_result_bytes.saturating_mul(4) / 5 {
                tracing::warn!(
                    used = result_bytes,
                    limit = max_result_bytes,
                    config_path = "limits.sqlite.maxResultBytes",
                    "SQLite result materialization is near its configured limit"
                );
                warned = true;
            }
            output.push(value);
        }
        rows.push(output);
    }
    Ok(QueryResult {
        columns,
        rows,
        changes: 0,
        last_insert_row_id: None,
    })
}

fn validate_result_size(result: &QueryResult, limit: usize) -> Result<(), VmSqliteError> {
    let mut used = result.columns.iter().map(String::len).sum::<usize>();
    for value in result.rows.iter().flatten() {
        used = used
            .checked_add(sql_value_bytes(value))
            .ok_or(VmSqliteError::ResultTooLarge {
                used: usize::MAX,
                limit,
            })?;
        if used > limit {
            return Err(VmSqliteError::ResultTooLarge { used, limit });
        }
    }
    if used >= limit.saturating_mul(4) / 5 {
        tracing::warn!(
            used,
            limit,
            config_path = "limits.sqlite.maxResultBytes",
            "SQLite result materialization is near its configured limit"
        );
    }
    Ok(())
}

fn verify_foreign_keys(result: &QueryResult, backend: &'static str) -> Result<(), VmSqliteError> {
    if result.rows == [vec![SqlValue::SqlInteger(1)]] {
        Ok(())
    } else {
        Err(VmSqliteError::ForeignKeysDisabled { backend })
    }
}

fn sql_value_bytes(value: &SqlValue) -> usize {
    match value {
        SqlValue::SqlNull => 0,
        SqlValue::SqlInteger(_) | SqlValue::SqlReal(_) => 8,
        SqlValue::SqlText(value) => value.len(),
        SqlValue::SqlBlob(value) => value.len(),
    }
}

fn sql_value_to_local(value: &SqlValue) -> Result<Value, VmSqliteError> {
    Ok(match value {
        SqlValue::SqlNull => Value::Null,
        SqlValue::SqlInteger(value) => Value::Integer(*value),
        SqlValue::SqlReal(value) if value.is_finite() => Value::Real(*value),
        SqlValue::SqlReal(_) => {
            return Err(VmSqliteError::InvalidResult(
                "SQLite real parameters must be finite".to_owned(),
            ));
        }
        SqlValue::SqlText(value) => Value::Text(value.clone()),
        SqlValue::SqlBlob(value) => Value::Blob(value.clone()),
    })
}

fn local_value_to_sql(value: ValueRef<'_>) -> Result<SqlValue, VmSqliteError> {
    Ok(match value {
        ValueRef::Null => SqlValue::SqlNull,
        ValueRef::Integer(value) => SqlValue::SqlInteger(value),
        ValueRef::Real(value) if value.is_finite() => SqlValue::SqlReal(value),
        ValueRef::Real(_) => {
            return Err(VmSqliteError::InvalidResult(
                "SQLite returned a non-finite real".to_owned(),
            ));
        }
        ValueRef::Text(value) => SqlValue::SqlText(
            std::str::from_utf8(value)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?
                .to_owned(),
        ),
        ValueRef::Blob(value) => SqlValue::SqlBlob(value.to_vec()),
    })
}

pub struct VmSqliteMigration {
    pub version: i64,
    pub statements: &'static [&'static str],
}

pub async fn migrate_schema(
    database: &dyn VmSqliteDatabase,
    owner: &str,
    version_table: &str,
    migrations: &[VmSqliteMigration],
) -> Result<(), VmSqliteError> {
    let expected_version_table = match owner {
        "filesystem" => "agentos_fs_schema_version",
        "core" => "agentos_core_schema_version",
        "actor" => "agentos_actor_schema_version",
        _ => {
            return Err(VmSqliteError::InvalidResult(format!(
                "unknown AgentOS SQLite schema owner {owner:?}"
            )))
        }
    };
    if version_table != expected_version_table {
        return Err(VmSqliteError::InvalidResult(format!(
            "SQLite schema owner {owner} must use {expected_version_table}, not {version_table}"
        )));
    }
    if version_table.is_empty()
        || !version_table
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return Err(VmSqliteError::InvalidResult(format!(
            "invalid schema version table {version_table:?}"
        )));
    }
    for (index, migration) in migrations.iter().enumerate() {
        let expected = i64::try_from(index).unwrap_or(i64::MAX) + 1;
        if migration.version != expected {
            return Err(VmSqliteError::InvalidMigrationLadder {
                component: owner.to_owned(),
                expected,
            });
        }
    }
    database
        .query(SqlStatement::plain(format!(
            "CREATE TABLE IF NOT EXISTS {version_table} (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), schema_version INTEGER NOT NULL CHECK (schema_version >= 0)) STRICT"
        )))
        .await?;
    let result = database
        .query(SqlStatement::plain(format!(
            "SELECT schema_version FROM {version_table} WHERE singleton = 1"
        )))
        .await?;
    let current = match result.rows.first().and_then(|row| row.first()) {
        None => 0,
        Some(SqlValue::SqlInteger(version)) => *version,
        Some(value) => {
            return Err(VmSqliteError::InvalidResult(format!(
                "schema version for {component} was not an integer: {value:?}",
                component = owner
            )));
        }
    };
    let supported = migrations
        .last()
        .map(|migration| migration.version)
        .unwrap_or(0);
    if current > supported {
        return Err(VmSqliteError::FutureSchema {
            component: owner.to_owned(),
            found: current,
            supported,
        });
    }
    for migration in migrations
        .iter()
        .filter(|migration| migration.version > current)
    {
        let mut statements = migration
            .statements
            .iter()
            .map(|sql| SqlStatement::plain(*sql))
            .collect::<Vec<_>>();
        statements.push(SqlStatement::new(
            format!("INSERT INTO {version_table} (singleton, schema_version) VALUES (1, ?) ON CONFLICT(singleton) DO UPDATE SET schema_version = excluded.schema_version"),
            vec![SqlValue::SqlInteger(migration.version)],
        ));
        database.transaction(statements).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> &'static agentos_runtime::SidecarRuntime {
        agentos_runtime::SidecarRuntime::process(&agentos_runtime::RuntimeConfig::default())
            .expect("runtime")
    }

    #[test]
    fn local_transactions_commit_and_roll_back() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("state.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar_core::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            database
                .transaction(vec![
                    SqlStatement::plain("CREATE TABLE values_table (value INTEGER NOT NULL)"),
                    SqlStatement::plain("INSERT INTO values_table VALUES (1)"),
                ])
                .await
                .expect("commit");
            let failed = database
                .transaction(vec![
                    SqlStatement::plain("INSERT INTO values_table VALUES (2)"),
                    SqlStatement::plain("INSERT INTO missing_table VALUES (3)"),
                ])
                .await;
            assert!(failed.is_err());
            let raced = database
                .transaction(vec![
                    SqlStatement::plain("INSERT INTO values_table VALUES (2)"),
                    SqlStatement::plain("UPDATE values_table SET value = 3 WHERE value = 99")
                        .expect_changes(1),
                ])
                .await;
            assert!(matches!(
                raced,
                Err(VmSqliteError::UnexpectedChanges {
                    expected: 1,
                    actual: 0,
                })
            ));
            let result = database
                .query(SqlStatement::plain("SELECT value FROM values_table"))
                .await
                .expect("query");
            assert_eq!(result.rows, vec![vec![SqlValue::SqlInteger(1)]]);
        });
    }

    #[test]
    fn local_migrations_keep_strict_owner_versions_independent() {
        const FS_MIGRATIONS: &[VmSqliteMigration] = &[
            VmSqliteMigration {
                version: 1,
                statements: &["CREATE TABLE agentos_fs_probe (value INTEGER NOT NULL) STRICT"],
            },
            VmSqliteMigration {
                version: 2,
                statements: &["CREATE TABLE agentos_fs_probe_v2 (value TEXT NOT NULL) STRICT"],
            },
        ];
        const CORE_MIGRATIONS: &[VmSqliteMigration] = &[VmSqliteMigration {
            version: 1,
            statements: &["CREATE TABLE agentos_core_probe (value BLOB NOT NULL) STRICT"],
        }];

        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("owner-versions.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar_core::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");

            let crossed_owner = migrate_schema(
                database.as_ref(),
                "core",
                "agentos_fs_schema_version",
                CORE_MIGRATIONS,
            )
            .await;
            assert!(matches!(crossed_owner, Err(VmSqliteError::InvalidResult(message)) if message.contains("must use agentos_core_schema_version")));
            let owner_side_effects = database
                .query(SqlStatement::plain(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'agentos_%_schema_version'",
                ))
                .await
                .expect("owner isolation side effects");
            assert_eq!(
                owner_side_effects.rows,
                vec![vec![SqlValue::SqlInteger(0)]]
            );

            migrate_schema(
                database.as_ref(),
                "core",
                "agentos_core_schema_version",
                CORE_MIGRATIONS,
            )
            .await
            .expect("core migration");
            migrate_schema(
                database.as_ref(),
                "filesystem",
                "agentos_fs_schema_version",
                FS_MIGRATIONS,
            )
            .await
            .expect("filesystem migration");

            let versions = database
                .query(SqlStatement::plain(
                    "SELECT (SELECT schema_version FROM agentos_fs_schema_version WHERE singleton = 1), (SELECT schema_version FROM agentos_core_schema_version WHERE singleton = 1), (SELECT COUNT(*) FROM sqlite_schema WHERE name = 'agentos_schema_versions')",
                ))
                .await
                .expect("owner versions");
            assert_eq!(
                versions.rows,
                vec![vec![
                    SqlValue::SqlInteger(2),
                    SqlValue::SqlInteger(1),
                    SqlValue::SqlInteger(0),
                ]]
            );

            let schemas = database
                .query(SqlStatement::plain(
                    "SELECT name, sql FROM sqlite_schema WHERE name IN ('agentos_fs_schema_version', 'agentos_fs_probe', 'agentos_fs_probe_v2', 'agentos_core_schema_version', 'agentos_core_probe') ORDER BY name",
                ))
                .await
                .expect("strict schemas");
            assert_eq!(schemas.rows.len(), 5);
            for row in schemas.rows {
                let Some(SqlValue::SqlText(name)) = row.first() else {
                    panic!("schema row was missing its table name: {row:?}");
                };
                let Some(SqlValue::SqlText(sql)) = row.get(1) else {
                    panic!("schema row for {name} was missing SQL: {row:?}");
                };
                assert!(
                    sql.trim_end().ends_with("STRICT"),
                    "owner table {name} was not STRICT: {sql}"
                );
            }

            let invalid_fs_type = database
                .query(SqlStatement::plain(
                    "INSERT INTO agentos_fs_probe (value) VALUES ('text')",
                ))
                .await;
            assert!(invalid_fs_type.is_err(), "STRICT fs table accepted TEXT");
            let invalid_core_type = database
                .query(SqlStatement::plain(
                    "INSERT INTO agentos_core_probe (value) VALUES (1)",
                ))
                .await;
            assert!(
                invalid_core_type.is_err(),
                "STRICT core table accepted INTEGER"
            );
        });
    }

    #[test]
    fn migration_validation_rejects_malformed_and_future_versions_without_changes() {
        const MALFORMED_MIGRATIONS: &[VmSqliteMigration] = &[
            VmSqliteMigration {
                version: 1,
                statements: &["CREATE TABLE agentos_fs_never_created (value INTEGER) STRICT"],
            },
            VmSqliteMigration {
                version: 3,
                statements: &["CREATE TABLE agentos_fs_also_never_created (value INTEGER) STRICT"],
            },
        ];
        const SUPPORTED_MIGRATIONS: &[VmSqliteMigration] = &[VmSqliteMigration {
            version: 1,
            statements: &["CREATE TABLE agentos_core_never_created (value INTEGER) STRICT"],
        }];

        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("invalid-versions.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar_core::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");

            let malformed = migrate_schema(
                database.as_ref(),
                "filesystem",
                "agentos_fs_schema_version",
                MALFORMED_MIGRATIONS,
            )
            .await;
            assert!(matches!(
                malformed,
                Err(VmSqliteError::InvalidMigrationLadder {
                    ref component,
                    expected: 2,
                }) if component == "filesystem"
            ));
            let malformed_changes = database
                .query(SqlStatement::plain(
                    "SELECT COUNT(*) FROM sqlite_schema WHERE name LIKE 'agentos_fs_%'",
                ))
                .await
                .expect("malformed ladder side effects");
            assert_eq!(
                malformed_changes.rows,
                vec![vec![SqlValue::SqlInteger(0)]]
            );

            database
                .transaction(vec![
                    SqlStatement::plain(
                        "CREATE TABLE agentos_core_schema_version (singleton INTEGER PRIMARY KEY CHECK (singleton = 1), schema_version INTEGER NOT NULL CHECK (schema_version >= 0)) STRICT",
                    ),
                    SqlStatement::plain(
                        "INSERT INTO agentos_core_schema_version (singleton, schema_version) VALUES (1, 2)",
                    ),
                ])
                .await
                .expect("future version fixture");
            let future = migrate_schema(
                database.as_ref(),
                "core",
                "agentos_core_schema_version",
                SUPPORTED_MIGRATIONS,
            )
            .await;
            assert!(matches!(
                future,
                Err(VmSqliteError::FutureSchema {
                    ref component,
                    found: 2,
                    supported: 1,
                }) if component == "core"
            ));
            let future_state = database
                .query(SqlStatement::plain(
                    "SELECT (SELECT schema_version FROM agentos_core_schema_version WHERE singleton = 1), (SELECT COUNT(*) FROM sqlite_schema WHERE name = 'agentos_core_never_created')",
                ))
                .await
                .expect("future version state");
            assert_eq!(
                future_state.rows,
                vec![vec![SqlValue::SqlInteger(2), SqlValue::SqlInteger(0)]]
            );
        });
    }

    #[test]
    fn local_migration_rolls_back_schema_and_version_atomically_on_strict_failure() {
        const MIGRATIONS: &[VmSqliteMigration] = &[VmSqliteMigration {
            version: 1,
            statements: &[
                "CREATE TABLE agentos_fs_atomic_probe (value INTEGER NOT NULL) STRICT",
                "INSERT INTO agentos_fs_atomic_probe (value) VALUES ('not-an-integer')",
            ],
        }];

        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("atomic-migration.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar_core::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");

            assert!(
                migrate_schema(
                    database.as_ref(),
                    "filesystem",
                    "agentos_fs_schema_version",
                    MIGRATIONS,
                )
                .await
                .is_err(),
                "STRICT type violation unexpectedly migrated"
            );
            let state = database
                .query(SqlStatement::plain(
                    "SELECT (SELECT COUNT(*) FROM sqlite_schema WHERE name = 'agentos_fs_atomic_probe'), (SELECT COUNT(*) FROM agentos_fs_schema_version)",
                ))
                .await
                .expect("atomic rollback state");
            assert_eq!(
                state.rows,
                vec![vec![SqlValue::SqlInteger(0), SqlValue::SqlInteger(0)]]
            );
        });
    }

    #[test]
    fn local_result_limit_rejects_queries_and_rolls_back_transactions() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("result-limit.sqlite").display().to_string(),
                },
                context,
                32,
            )
            .await
            .expect("database");
            database
                .transaction(vec![
                    SqlStatement::plain(
                        "CREATE TABLE values_table (id INTEGER PRIMARY KEY, value TEXT NOT NULL)",
                    ),
                    SqlStatement::new(
                        "INSERT INTO values_table (id, value) VALUES (?, ?)",
                        vec![SqlValue::SqlInteger(1), SqlValue::SqlText("x".repeat(64))],
                    ),
                ])
                .await
                .expect("setup");

            let oversized = database
                .query(SqlStatement::plain(
                    "SELECT value FROM values_table WHERE id = 1",
                ))
                .await;
            assert!(matches!(
                oversized,
                Err(VmSqliteError::ResultTooLarge { limit: 32, .. })
            ));

            let failed = database
                .transaction(vec![
                    SqlStatement::new(
                        "INSERT INTO values_table (id, value) VALUES (?, ?)",
                        vec![SqlValue::SqlInteger(2), SqlValue::SqlText("y".repeat(64))],
                    ),
                    SqlStatement::plain("SELECT value FROM values_table WHERE id = 2"),
                ])
                .await;
            assert!(matches!(
                failed,
                Err(VmSqliteError::ResultTooLarge { limit: 32, .. })
            ));
            let result = database
                .query(SqlStatement::plain("SELECT COUNT(*) FROM values_table"))
                .await
                .expect("count after rollback");
            assert_eq!(result.rows, vec![vec![SqlValue::SqlInteger(1)]]);
        });
    }

    #[test]
    fn foreign_key_verification_requires_enabled_pragma() {
        let enabled = QueryResult {
            columns: vec![String::from("foreign_keys")],
            rows: vec![vec![SqlValue::SqlInteger(1)]],
            changes: 0,
            last_insert_row_id: None,
        };
        verify_foreign_keys(&enabled, "test").expect("enabled");

        let disabled = QueryResult {
            rows: vec![vec![SqlValue::SqlInteger(0)]],
            ..enabled
        };
        assert!(matches!(
            verify_foreign_keys(&disabled, "actor_uds"),
            Err(VmSqliteError::ForeignKeysDisabled {
                backend: "actor_uds"
            })
        ));
    }
}
