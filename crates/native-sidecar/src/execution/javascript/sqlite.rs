use super::super::*;

const SQLITE_JS_SAFE_INTEGER_MAX: i64 = 9_007_199_254_740_991;

pub(in crate::execution) fn service_javascript_sqlite_sync_rpc(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    match request.method.as_str() {
        "sqlite.constants" => Ok(json!({})),
        "sqlite.open" => sqlite_open_database(kernel, process, request),
        "sqlite.close" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.close database id")?;
            close_sqlite_database(kernel, process, database_id, false)?;
            Ok(Value::Null)
        }
        "sqlite.exec" => sqlite_exec_database(kernel, process, request),
        "sqlite.query" => sqlite_query_database(kernel, process, request),
        "sqlite.prepare" => sqlite_prepare_statement(process, request),
        "sqlite.location" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.location database id")?;
            let database = sqlite_database(process, database_id)?;
            Ok(database
                .vm_path
                .as_ref()
                .map(|path| Value::String(path.clone()))
                .unwrap_or(Value::Null))
        }
        "sqlite.checkpoint" => {
            let database_id =
                javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.checkpoint database id")?;
            let kernel_pid = process.kernel_pid;
            let database = sqlite_database_mut(process, database_id)?;
            sqlite_sync_database(kernel, kernel_pid, database, false)?;
            Ok(Value::Null)
        }
        "sqlite.statement.run" => sqlite_run_statement(kernel, process, request),
        "sqlite.statement.get" => sqlite_get_statement(kernel, process, request),
        "sqlite.statement.all" | "sqlite.statement.iterate" => {
            sqlite_all_statement(kernel, process, request)
        }
        "sqlite.statement.columns" => sqlite_statement_columns(process, request),
        "sqlite.statement.setReturnArrays" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setReturnArrays statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setReturnArrays enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.return_arrays = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setReadBigInts" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setReadBigInts statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setReadBigInts enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.read_bigints = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setAllowBareNamedParameters" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setAllowBareNamedParameters statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setAllowBareNamedParameters enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.allow_bare_named_parameters = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.setAllowUnknownNamedParameters" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.setAllowUnknownNamedParameters statement id",
            )?;
            let enabled = javascript_sync_rpc_arg_bool(
                &request.args,
                1,
                "sqlite.statement.setAllowUnknownNamedParameters enabled",
            )?;
            sqlite_statement_mut(process, statement_id)?.allow_unknown_named_parameters = enabled;
            Ok(Value::Null)
        }
        "sqlite.statement.finalize" => {
            let statement_id = javascript_sync_rpc_arg_u64(
                &request.args,
                0,
                "sqlite.statement.finalize statement id",
            )?;
            process
                .sqlite_statements
                .remove(&statement_id)
                .ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "sqlite statement handle not found: {statement_id}"
                    ))
                })?;
            Ok(Value::Null)
        }
        other => Err(SidecarError::InvalidState(format!(
            "unsupported JavaScript sqlite sync RPC method {other}"
        ))),
    }
}

fn sqlite_open_database(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    ensure_per_process_state_handle_capacity(process.sqlite_databases.len(), "sqlite database")?;
    let path = request.args.first().and_then(Value::as_str);
    let vm_path = path.filter(|value| !value.is_empty() && *value != ":memory:");
    let options = request.args.get(1);
    let read_only = sqlite_option_bool(options, "readOnly").unwrap_or(false);
    let create = sqlite_option_bool(options, "create").unwrap_or(!read_only);
    let timeout_ms = sqlite_option_u64(options, "timeout");

    process.next_sqlite_database_id += 1;
    let database_id = process.next_sqlite_database_id;

    let host_path = vm_path.map(|vm_path| {
        let digest = Sha256::digest(vm_path.as_bytes());
        let path_key = digest
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::env::temp_dir()
            .join(format!(
                "agentos-native-sidecar-sqlite-{}",
                process.sqlite_host_namespace
            ))
            .join(path_key)
            .join("database.sqlite")
    });
    let host_database_exists = host_path.as_ref().is_some_and(|path| path.exists());

    if let Some(host_path) = host_path.as_ref() {
        if let Some(parent) = host_path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to prepare sqlite temp directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
    }

    if !host_database_exists {
        if let (Some(vm_path), Some(host_path)) = (vm_path, host_path.as_ref()) {
            if kernel
                .exists_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, vm_path)
                .map_err(kernel_error)?
            {
                let contents = kernel
                    .read_file_for_process(EXECUTION_DRIVER_NAME, process.kernel_pid, vm_path)
                    .map_err(kernel_error)?;
                fs::write(host_path, contents).map_err(|error| {
                    SidecarError::Io(format!(
                        "failed to materialize sqlite database {}: {error}",
                        host_path.display()
                    ))
                })?;
            } else if read_only && !create {
                return Err(SidecarError::InvalidState(format!(
                    "sqlite database does not exist: {vm_path}"
                )));
            }
        }
    }

    let target = host_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from(":memory:"));
    let mut flags = if read_only {
        SqliteOpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        SqliteOpenFlags::SQLITE_OPEN_READ_WRITE
    };
    if create && !read_only {
        flags |= SqliteOpenFlags::SQLITE_OPEN_CREATE;
    }

    let connection = SqliteConnection::open_with_flags(&target, flags).map_err(|error| {
        SidecarError::InvalidState(format!(
            "sqlite database open failed for {}: {error}",
            vm_path.unwrap_or(":memory:")
        ))
    })?;
    if let Some(timeout_ms) = timeout_ms {
        connection
            .busy_timeout(Duration::from_millis(timeout_ms))
            .map_err(sqlite_error)?;
    }
    if host_path.is_some() && !read_only {
        let _ = connection.pragma_update(None, "journal_mode", "WAL");
    }

    process.sqlite_databases.insert(
        database_id,
        ActiveSqliteDatabase {
            connection,
            host_path,
            vm_path: vm_path.map(String::from),
            dirty: false,
            transaction_depth: 0,
            read_only,
        },
    );
    Ok(json!(database_id))
}

fn sqlite_exec_database(
    _kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.exec database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.exec sql")?;
    let database = sqlite_database_mut(process, database_id)?;
    let before = database.connection.total_changes();
    database
        .connection
        .execute_batch(sql)
        .map_err(sqlite_error)?;
    mark_sqlite_mutation(database, sql);
    Ok(json!(database
        .connection
        .total_changes()
        .saturating_sub(before)))
}

fn sqlite_query_database(
    _kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.query database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.query sql")?;
    let params = request.args.get(2);
    let options = request.args.get(3);
    let return_arrays = sqlite_option_bool(options, "returnArrays").unwrap_or(false);
    let read_bigints = sqlite_option_bool(options, "readBigInts").unwrap_or(false);
    let database = sqlite_database_mut(process, database_id)?;
    let rows = sqlite_query_rows(
        &mut database.connection,
        sql,
        params,
        return_arrays,
        read_bigints,
        true,
        false,
    )?;
    mark_sqlite_mutation(database, sql);
    Ok(rows)
}

fn sqlite_prepare_statement(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    ensure_per_process_state_handle_capacity(process.sqlite_statements.len(), "sqlite statement")?;
    let database_id = javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.prepare database id")?;
    let sql = javascript_sync_rpc_arg_str(&request.args, 1, "sqlite.prepare sql")?;
    let _ = sqlite_database(process, database_id)?;
    process.next_sqlite_statement_id += 1;
    let statement_id = process.next_sqlite_statement_id;
    process.sqlite_statements.insert(
        statement_id,
        ActiveSqliteStatement {
            database_id,
            sql: sql.to_owned(),
            return_arrays: false,
            read_bigints: false,
            allow_bare_named_parameters: false,
            allow_unknown_named_parameters: false,
        },
    );
    Ok(json!(statement_id))
}

fn sqlite_run_statement(
    _kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.run statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let before = database.connection.total_changes();
    {
        let mut statement = database
            .connection
            .prepare(&statement_state.sql)
            .map_err(sqlite_error)?;
        bind_sqlite_parameters(
            &mut statement,
            params,
            statement_state.allow_bare_named_parameters,
            statement_state.allow_unknown_named_parameters,
        )?;
        statement.raw_execute().map_err(sqlite_error)?;
    }
    let changes = database.connection.total_changes().saturating_sub(before);
    let last_insert_rowid = database.connection.last_insert_rowid();
    mark_sqlite_mutation(database, &statement_state.sql);
    let result = json!({
        "changes": changes,
        "lastInsertRowid": encode_sqlite_integer(last_insert_rowid, true),
    });
    Ok(result)
}

fn sqlite_get_statement(
    _kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.get statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let rows = sqlite_query_rows(
        &mut database.connection,
        &statement_state.sql,
        params,
        statement_state.return_arrays,
        statement_state.read_bigints,
        statement_state.allow_bare_named_parameters,
        statement_state.allow_unknown_named_parameters,
    )?;
    mark_sqlite_mutation(database, &statement_state.sql);
    Ok(rows
        .as_array()
        .and_then(|rows| rows.first().cloned())
        .unwrap_or(Value::Null))
}

fn sqlite_all_statement(
    _kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.all statement id")?;
    let params = request.args.get(1);
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let rows = sqlite_query_rows(
        &mut database.connection,
        &statement_state.sql,
        params,
        statement_state.return_arrays,
        statement_state.read_bigints,
        statement_state.allow_bare_named_parameters,
        statement_state.allow_unknown_named_parameters,
    )?;
    mark_sqlite_mutation(database, &statement_state.sql);
    Ok(rows)
}

fn sqlite_statement_columns(
    process: &mut ActiveProcess,
    request: &JavascriptSyncRpcRequest,
) -> Result<Value, SidecarError> {
    let statement_id =
        javascript_sync_rpc_arg_u64(&request.args, 0, "sqlite.statement.columns statement id")?;
    let statement_state = sqlite_statement(process, statement_id)?.clone();
    let database = sqlite_database_mut(process, statement_state.database_id)?;
    let statement = database
        .connection
        .prepare(&statement_state.sql)
        .map_err(sqlite_error)?;
    Ok(Value::Array(
        statement
            .column_names()
            .iter()
            .map(|name| json!({ "name": name }))
            .collect(),
    ))
}

fn sqlite_query_rows(
    connection: &mut SqliteConnection,
    sql: &str,
    params: Option<&Value>,
    return_arrays: bool,
    read_bigints: bool,
    allow_bare_named_parameters: bool,
    allow_unknown_named_parameters: bool,
) -> Result<Value, SidecarError> {
    let mut statement = connection.prepare(sql).map_err(sqlite_error)?;
    let column_names = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let column_count = statement.column_count();
    bind_sqlite_parameters(
        &mut statement,
        params,
        allow_bare_named_parameters,
        allow_unknown_named_parameters,
    )?;
    let mut rows = statement.raw_query();
    let mut encoded_rows = Vec::new();
    while let Some(row) = rows.next().map_err(sqlite_error)? {
        encoded_rows.push(encode_sqlite_row(
            row,
            &column_names,
            column_count,
            return_arrays,
            read_bigints,
        )?);
    }
    Ok(Value::Array(encoded_rows))
}

fn encode_sqlite_row(
    row: &rusqlite::Row<'_>,
    column_names: &[String],
    column_count: usize,
    return_arrays: bool,
    read_bigints: bool,
) -> Result<Value, SidecarError> {
    if return_arrays {
        let mut values = Vec::with_capacity(column_count);
        for index in 0..column_count {
            values.push(encode_sqlite_value_ref(
                row.get_ref(index).map_err(sqlite_error)?,
                read_bigints,
            )?);
        }
        return Ok(Value::Array(values));
    }

    let mut object = Map::with_capacity(column_count);
    for (index, name) in column_names.iter().enumerate() {
        object.insert(
            name.clone(),
            encode_sqlite_value_ref(row.get_ref(index).map_err(sqlite_error)?, read_bigints)?,
        );
    }
    Ok(Value::Object(object))
}

fn encode_sqlite_value_ref(
    value: SqliteValueRef<'_>,
    read_bigints: bool,
) -> Result<Value, SidecarError> {
    Ok(match value {
        SqliteValueRef::Null => Value::Null,
        SqliteValueRef::Integer(number) => encode_sqlite_integer(number, read_bigints),
        SqliteValueRef::Real(number) => json!(number),
        SqliteValueRef::Text(text) => Value::String(String::from_utf8_lossy(text).into_owned()),
        SqliteValueRef::Blob(bytes) => json!({
            "__agentosSqliteType": "uint8array",
            "value": base64::engine::general_purpose::STANDARD.encode(bytes),
        }),
    })
}

fn encode_sqlite_integer(number: i64, read_bigints: bool) -> Value {
    if read_bigints || number.abs() > SQLITE_JS_SAFE_INTEGER_MAX {
        json!({
            "__agentosSqliteType": "bigint",
            "value": number.to_string(),
        })
    } else {
        json!(number)
    }
}

fn bind_sqlite_parameters(
    statement: &mut SqliteStatement<'_>,
    params: Option<&Value>,
    allow_bare_named_parameters: bool,
    allow_unknown_named_parameters: bool,
) -> Result<(), SidecarError> {
    let Some(params) = params else {
        return Ok(());
    };
    match params {
        Value::Null => Ok(()),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                statement
                    .raw_bind_parameter(index + 1, decode_sqlite_parameter(value)?)
                    .map_err(sqlite_error)?;
            }
            Ok(())
        }
        Value::Object(map)
            if map
                .get("__agentosSqliteType")
                .and_then(Value::as_str)
                .is_none() =>
        {
            for (key, value) in map {
                let index =
                    resolve_sqlite_parameter_index(statement, key, allow_bare_named_parameters)?;
                let Some(index) = index else {
                    if allow_unknown_named_parameters {
                        continue;
                    }
                    return Err(SidecarError::InvalidState(format!(
                        "sqlite named parameter not found: {key}"
                    )));
                };
                statement
                    .raw_bind_parameter(index, decode_sqlite_parameter(value)?)
                    .map_err(sqlite_error)?;
            }
            Ok(())
        }
        other => statement
            .raw_bind_parameter(1, decode_sqlite_parameter(other)?)
            .map_err(sqlite_error),
    }
}

fn resolve_sqlite_parameter_index(
    statement: &mut SqliteStatement<'_>,
    key: &str,
    allow_bare_named_parameters: bool,
) -> Result<Option<usize>, SidecarError> {
    let mut candidates = vec![key.to_owned()];
    if allow_bare_named_parameters
        && !key.starts_with(':')
        && !key.starts_with('@')
        && !key.starts_with('$')
    {
        candidates.push(format!(":{key}"));
        candidates.push(format!("@{key}"));
        candidates.push(format!("${key}"));
    }
    for candidate in candidates {
        if let Some(index) = statement
            .parameter_index(&candidate)
            .map_err(sqlite_error)?
        {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn decode_sqlite_parameter(value: &Value) -> Result<rusqlite::types::Value, SidecarError> {
    Ok(match value {
        Value::Null => rusqlite::types::Value::Null,
        Value::Bool(value) => rusqlite::types::Value::Integer(i64::from(*value)),
        Value::Number(value) => match (value.as_i64(), value.as_f64()) {
            (Some(integer), _) => rusqlite::types::Value::Integer(integer),
            (_, Some(real)) => rusqlite::types::Value::Real(real),
            _ => {
                return Err(SidecarError::InvalidState(String::from(
                    "sqlite parameter number is not representable",
                )));
            }
        },
        Value::String(value) => rusqlite::types::Value::Text(value.clone()),
        Value::Array(_) => {
            return Err(SidecarError::InvalidState(String::from(
                "sqlite parameters do not support nested arrays",
            )));
        }
        Value::Object(map) => match map.get("__agentosSqliteType").and_then(Value::as_str) {
            Some("bigint") => rusqlite::types::Value::Integer(
                map.get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "sqlite bigint parameter missing string value",
                        ))
                    })?
                    .parse::<i64>()
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "sqlite bigint parameter is not a signed 64-bit integer: {error}"
                        ))
                    })?,
            ),
            Some("uint8array") => rusqlite::types::Value::Blob(
                base64::engine::general_purpose::STANDARD
                    .decode(map.get("value").and_then(Value::as_str).ok_or_else(|| {
                        SidecarError::InvalidState(String::from(
                            "sqlite blob parameter missing base64 value",
                        ))
                    })?)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "sqlite blob parameter contains invalid base64: {error}"
                        ))
                    })?,
            ),
            Some(other) => {
                return Err(SidecarError::InvalidState(format!(
                    "unsupported sqlite tagged parameter type {other}"
                )));
            }
            None => {
                return Err(SidecarError::InvalidState(String::from(
                    "sqlite named parameter objects must be passed as the top-level params object",
                )));
            }
        },
    })
}

pub(in crate::execution) fn close_sqlite_database(
    kernel: &mut SidecarKernel,
    process: &mut ActiveProcess,
    database_id: u64,
    process_exited: bool,
) -> Result<(), SidecarError> {
    let mut database = process
        .sqlite_databases
        .remove(&database_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
        })?;
    process
        .sqlite_statements
        .retain(|_, statement| statement.database_id != database_id);
    let host_path = database.host_path.clone();
    let writable_sibling_database_id = host_path.as_ref().and_then(|closed_path| {
        process
            .sqlite_databases
            .iter()
            .find(|(_, database)| {
                database.host_path.as_ref() == Some(closed_path) && !database.read_only
            })
            .map(|(database_id, _)| *database_id)
    });
    let host_path_still_open = host_path.as_ref().is_some_and(|closed_path| {
        process
            .sqlite_databases
            .values()
            .any(|database| database.host_path.as_ref() == Some(closed_path))
    });
    let sync_result = if database.dirty && database.transaction_depth == 0 {
        if let Some(sibling_database_id) = writable_sibling_database_id {
            process
                .sqlite_databases
                .get_mut(&sibling_database_id)
                .expect("writable sqlite sibling")
                .dirty = true;
            Ok(())
        } else {
            // A read-only sibling cannot own a later snapshot. Persist committed
            // changes from this writable handle before dropping it, even though
            // the shared host database remains open for readers.
            sqlite_sync_database(kernel, process.kernel_pid, &mut database, process_exited)
        }
    } else if !host_path_still_open {
        sqlite_sync_database(kernel, process.kernel_pid, &mut database, process_exited)
    } else {
        Ok(())
    };
    if let Err(error) = sync_result {
        // A DatabaseSync.close() can race process exit: the process identity is
        // no longer valid for the live per-process VFS write even though its
        // bridge RPC is still draining. Retain the handle so process teardown
        // can retry copy-back with its already-authorized VM teardown path.
        process.sqlite_databases.insert(database_id, database);
        return Err(error);
    }
    drop(database);
    if !host_path_still_open {
        cleanup_sqlite_host_artifacts(host_path.as_deref())?;
    }
    Ok(())
}

pub(in crate::execution) fn ensure_per_process_state_handle_capacity(
    len: usize,
    label: &str,
) -> Result<(), SidecarError> {
    if len >= MAX_PER_PROCESS_STATE_HANDLES {
        return Err(SidecarError::InvalidState(format!(
            "{label} handle limit exceeded: limit is {MAX_PER_PROCESS_STATE_HANDLES}"
        )));
    }
    Ok(())
}

fn sqlite_sync_database(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    database: &mut ActiveSqliteDatabase,
    process_exited: bool,
) -> Result<(), SidecarError> {
    if !database.dirty
        || database.transaction_depth > 0
        || database.read_only
        || database.host_path.is_none()
        || database.vm_path.is_none()
    {
        return Ok(());
    }

    let host_path = database.host_path.as_ref().expect("sqlite host path");
    if !host_path.exists() {
        return Ok(());
    }
    // The main file alone is not a consistent snapshot when the guest selected
    // WAL mode. A checkpoint can remain busy while OpenCode keeps prepared read
    // statements alive, so persisting only the main file silently loses schema
    // and session rows from the WAL. SQLite's online backup API includes committed
    // WAL pages without invalidating live statements.
    let snapshot_path = PathBuf::from(format!("{}.snapshot", host_path.display()));
    if snapshot_path.exists() {
        fs::remove_file(&snapshot_path).map_err(|error| {
            SidecarError::Io(format!(
                "failed to remove stale sqlite snapshot {}: {error}",
                snapshot_path.display()
            ))
        })?;
    }
    let mut snapshot = SqliteConnection::open(&snapshot_path).map_err(sqlite_error)?;
    {
        let backup =
            SqliteBackup::new(&database.connection, &mut snapshot).map_err(sqlite_error)?;
        backup
            .run_to_completion(256, Duration::from_millis(1), None)
            .map_err(sqlite_error)?;
    }
    drop(snapshot);
    let vm_path = database.vm_path.as_deref().expect("sqlite vm path");
    if process_exited {
        ensure_vm_parent_dir_unchecked(kernel, vm_path)?;
    } else {
        ensure_vm_parent_dir(kernel, kernel_pid, vm_path)?;
    }
    let contents = fs::read(&snapshot_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to read sqlite snapshot {}: {error}",
            snapshot_path.display()
        ))
    })?;
    if process_exited {
        // The process already opened this database for write while its DAC and
        // mount permissions were live. Exit cleanup runs after that process is
        // no longer a valid kernel authorization subject, so copy the completed
        // online-backup snapshot through the VM root before the VM snapshot.
        kernel.write_file(vm_path, contents).map_err(kernel_error)?;
    } else {
        kernel
            .write_file_for_process(EXECUTION_DRIVER_NAME, kernel_pid, vm_path, contents, None)
            .map_err(kernel_error)?;
    }
    fs::remove_file(&snapshot_path).map_err(|error| {
        SidecarError::Io(format!(
            "failed to remove sqlite snapshot {}: {error}",
            snapshot_path.display()
        ))
    })?;
    database.dirty = false;
    Ok(())
}

fn cleanup_sqlite_host_artifacts(host_path: Option<&Path>) -> Result<(), SidecarError> {
    let Some(host_path) = host_path else {
        return Ok(());
    };
    let parent = host_path.parent().map(PathBuf::from);
    for suffix in ["", "-wal", "-shm", ".snapshot"] {
        let path = PathBuf::from(format!("{}{}", host_path.display(), suffix));
        if path.exists() {
            fs::remove_file(&path).map_err(|error| {
                SidecarError::Io(format!(
                    "failed to remove sqlite temp artifact {}: {error}",
                    path.display()
                ))
            })?;
        }
    }
    if let Some(parent) = parent {
        let _ = fs::remove_dir_all(parent);
    }
    Ok(())
}

fn ensure_vm_parent_dir(
    kernel: &mut SidecarKernel,
    kernel_pid: u32,
    path: &str,
) -> Result<(), SidecarError> {
    let parent = dirname(path);
    if parent == "/" || parent == "." {
        return Ok(());
    }
    let mut current = String::new();
    for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
        current.push('/');
        current.push_str(segment);
        if !kernel
            .exists_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &current)
            .map_err(kernel_error)?
        {
            kernel
                .mkdir_for_process(EXECUTION_DRIVER_NAME, kernel_pid, &current, false, None)
                .map_err(kernel_error)?;
        }
    }
    Ok(())
}

fn ensure_vm_parent_dir_unchecked(
    kernel: &mut SidecarKernel,
    path: &str,
) -> Result<(), SidecarError> {
    let parent = dirname(path);
    if parent == "/" || parent == "." {
        return Ok(());
    }
    let mut current = String::new();
    for segment in parent.split('/').filter(|segment| !segment.is_empty()) {
        current.push('/');
        current.push_str(segment);
        if !kernel.exists(&current).map_err(kernel_error)? {
            kernel.create_dir(&current).map_err(kernel_error)?;
        }
    }
    Ok(())
}

fn sqlite_database(
    process: &ActiveProcess,
    database_id: u64,
) -> Result<&ActiveSqliteDatabase, SidecarError> {
    process.sqlite_databases.get(&database_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
    })
}

fn sqlite_database_mut(
    process: &mut ActiveProcess,
    database_id: u64,
) -> Result<&mut ActiveSqliteDatabase, SidecarError> {
    process
        .sqlite_databases
        .get_mut(&database_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite database handle not found: {database_id}"))
        })
}

fn sqlite_statement(
    process: &ActiveProcess,
    statement_id: u64,
) -> Result<&ActiveSqliteStatement, SidecarError> {
    process.sqlite_statements.get(&statement_id).ok_or_else(|| {
        SidecarError::InvalidState(format!("sqlite statement handle not found: {statement_id}"))
    })
}

fn sqlite_statement_mut(
    process: &mut ActiveProcess,
    statement_id: u64,
) -> Result<&mut ActiveSqliteStatement, SidecarError> {
    process
        .sqlite_statements
        .get_mut(&statement_id)
        .ok_or_else(|| {
            SidecarError::InvalidState(format!("sqlite statement handle not found: {statement_id}"))
        })
}

fn mark_sqlite_mutation(database: &mut ActiveSqliteDatabase, sql: &str) {
    let normalized = sql.trim_start().to_ascii_lowercase();
    if normalized.starts_with("begin") || normalized.starts_with("savepoint") {
        database.dirty = true;
        database.transaction_depth += 1;
        return;
    }
    if normalized.starts_with("commit") || normalized.starts_with("release savepoint") {
        database.dirty = true;
        database.transaction_depth = database.transaction_depth.saturating_sub(1);
        return;
    }
    if normalized.starts_with("rollback") && !normalized.starts_with("rollback to") {
        database.dirty = true;
        database.transaction_depth = database.transaction_depth.saturating_sub(1);
        return;
    }
    if normalized.starts_with("insert")
        || normalized.starts_with("update")
        || normalized.starts_with("delete")
        || normalized.starts_with("replace")
        || normalized.starts_with("create")
        || normalized.starts_with("alter")
        || normalized.starts_with("drop")
        || normalized.starts_with("vacuum")
        || normalized.starts_with("reindex")
        || normalized.starts_with("analyze")
        || normalized.starts_with("attach")
        || normalized.starts_with("detach")
        || normalized.starts_with("pragma")
    {
        database.dirty = true;
    }
}

fn sqlite_option_bool(options: Option<&Value>, key: &str) -> Option<bool> {
    options
        .and_then(|value| value.get(key))
        .and_then(Value::as_bool)
}

fn sqlite_option_u64(options: Option<&Value>, key: &str) -> Option<u64> {
    options
        .and_then(|value| value.get(key))
        .and_then(Value::as_u64)
}

fn sqlite_error(error: rusqlite::Error) -> SidecarError {
    SidecarError::InvalidState(format!("sqlite error: {error}"))
}
