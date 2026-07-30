use agent_client_protocol_schema::v1::{
    RequestPermissionRequest as AcpRequestPermissionRequest,
    RequestPermissionResponse as AcpRequestPermissionResponse, SessionUpdate as AcpSessionUpdate,
};
use agentos_native_sidecar::limits::AcpLimits;
use agentos_native_sidecar::vm_sqlite::{
    migrate_schema, QueryResult, SharedVmSqliteDatabase, SqlStatement, SqlValue, VmSqliteError,
    VmSqliteMigration,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_SAFE_SEQUENCE: i64 = 9_007_199_254_740_991;
const SESSION_MIGRATION_1: &[&str] = &[
    "CREATE TABLE agentos_core_sessions (\
      session_id TEXT PRIMARY KEY CHECK (length(session_id) BETWEEN 1 AND 256), \
      agent TEXT NOT NULL CHECK (length(agent) BETWEEN 1 AND 256), acp_session_id TEXT, \
      state TEXT NOT NULL CHECK (state IN ('idle', 'running', 'waiting', 'failed')), \
      state_prompt_id TEXT, state_started_at_ms INTEGER, cwd TEXT NOT NULL CHECK (substr(cwd, 1, 1) = '/'), \
      permission_policy TEXT NOT NULL CHECK (permission_policy IN ('allow_all', 'reject_all', 'ask')), \
      skip_os_instructions INTEGER NOT NULL CHECK (skip_os_instructions IN (0, 1)), additional_instructions TEXT, \
      additional_directories_json TEXT NOT NULL CHECK (json_valid(additional_directories_json) AND json_type(additional_directories_json) = 'array'), \
      env_json TEXT NOT NULL CHECK (json_valid(env_json) AND json_type(env_json) = 'object'), \
      mcp_servers_json TEXT NOT NULL CHECK (json_valid(mcp_servers_json) AND json_type(mcp_servers_json) = 'array'), \
      capabilities_json TEXT CHECK (capabilities_json IS NULL OR json_valid(capabilities_json)), \
      agent_info_json TEXT CHECK (agent_info_json IS NULL OR json_valid(agent_info_json)), \
      config_revision INTEGER NOT NULL DEFAULT 0 CHECK (config_revision BETWEEN 0 AND 9007199254740991), \
      config_options_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(config_options_json) AND json_type(config_options_json) = 'array'), \
      title TEXT, metadata_json TEXT CHECK (metadata_json IS NULL OR json_valid(metadata_json)), \
      latest_sequence INTEGER NOT NULL DEFAULT 0 CHECK (latest_sequence BETWEEN 0 AND 9007199254740991), \
      oldest_retained_sequence INTEGER NOT NULL DEFAULT 1 CHECK (oldest_retained_sequence BETWEEN 1 AND 9007199254740991), \
      retained_event_count INTEGER NOT NULL DEFAULT 0 CHECK (retained_event_count >= 0), \
      retained_event_bytes INTEGER NOT NULL DEFAULT 0 CHECK (retained_event_bytes >= 0), \
      created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0), updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms), \
      CHECK ((state = 'idle' AND state_prompt_id IS NULL AND state_started_at_ms IS NULL) OR (state IN ('running', 'waiting', 'failed') AND state_prompt_id IS NOT NULL AND state_started_at_ms IS NOT NULL))) STRICT",
    "CREATE TABLE agentos_core_events (\
      session_id TEXT NOT NULL, sequence INTEGER NOT NULL CHECK (sequence BETWEEN 1 AND 9007199254740991), \
      occurred_at_ms INTEGER NOT NULL CHECK (occurred_at_ms >= 0), acp_protocol_version INTEGER NOT NULL CHECK (acp_protocol_version >= 1), \
      event_kind TEXT NOT NULL CHECK (event_kind IN ('session_update', 'permission_request', 'permission_response')), \
      correlation_id TEXT CHECK (correlation_id IS NULL OR length(correlation_id) BETWEEN 1 AND 256), \
      payload_json TEXT NOT NULL CHECK (json_valid(payload_json)), \
      payload_bytes INTEGER NOT NULL CHECK (payload_bytes = length(CAST(payload_json AS BLOB))), outcome_status TEXT, \
      terminal_reason TEXT CHECK (terminal_reason IS NULL OR terminal_reason IN ('already_resolved', 'prompt_cancelled', 'adapter_exited', 'session_deleted', 'vm_shutdown')), \
      PRIMARY KEY (session_id, sequence), \
      CHECK ((event_kind = 'session_update' AND correlation_id IS NULL AND outcome_status IS NULL AND terminal_reason IS NULL) OR \
             (event_kind = 'permission_request' AND correlation_id IS NOT NULL AND outcome_status IS NULL AND terminal_reason IS NULL) OR \
             (event_kind = 'permission_response' AND correlation_id IS NOT NULL AND outcome_status IN ('accepted', 'not_pending') AND \
              ((outcome_status = 'accepted' AND terminal_reason IS NULL) OR (outcome_status = 'not_pending' AND terminal_reason IS NOT NULL))))) STRICT",
    "CREATE TABLE agentos_core_prompts (\
      session_id TEXT NOT NULL, prompt_id TEXT NOT NULL, idempotency_key TEXT, \
      input_hash BLOB NOT NULL CHECK (length(input_hash) = 32), \
      state TEXT NOT NULL CHECK (state IN ('accepted', 'completed', 'failed', 'cancelled')), \
      result_json TEXT CHECK (result_json IS NULL OR json_valid(result_json)), \
      error_json TEXT CHECK (error_json IS NULL OR json_valid(error_json)), \
      first_input_sequence INTEGER CHECK (first_input_sequence BETWEEN 1 AND 9007199254740991), \
      last_output_sequence INTEGER CHECK (last_output_sequence BETWEEN 1 AND 9007199254740991), \
      created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0), updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms), \
      PRIMARY KEY (session_id, prompt_id), UNIQUE (session_id, idempotency_key), \
      CHECK ((state = 'completed' AND result_json IS NOT NULL AND error_json IS NULL) OR \
             (state IN ('failed', 'cancelled') AND error_json IS NOT NULL AND result_json IS NULL) OR \
             (state = 'accepted' AND result_json IS NULL AND error_json IS NULL))) STRICT",
    "CREATE TABLE agentos_core_permission_records (\
      session_id TEXT NOT NULL, request_id TEXT NOT NULL, prompt_id TEXT NOT NULL, \
      request_kind TEXT NOT NULL CHECK (request_kind = 'permission'), \
      state TEXT NOT NULL CHECK (state IN ('pending', 'responded', 'terminal')), \
      request_json TEXT NOT NULL CHECK (json_valid(request_json)), \
      response_json TEXT CHECK (response_json IS NULL OR json_valid(response_json)), \
      terminal_reason TEXT CHECK (terminal_reason IS NULL OR terminal_reason IN ('accepted', 'already_resolved', 'prompt_cancelled', 'adapter_exited', 'session_deleted', 'vm_shutdown')), \
      terminal_sequence INTEGER CHECK (terminal_sequence BETWEEN 1 AND 9007199254740991), \
      created_at_ms INTEGER NOT NULL CHECK (created_at_ms >= 0), updated_at_ms INTEGER NOT NULL CHECK (updated_at_ms >= created_at_ms), \
      PRIMARY KEY (session_id, request_id), \
      CHECK ((state = 'pending' AND response_json IS NULL AND terminal_reason IS NULL AND terminal_sequence IS NULL) OR \
             (state = 'responded' AND response_json IS NOT NULL AND terminal_reason IS NOT NULL) OR \
             (state = 'terminal' AND terminal_reason IS NOT NULL))) STRICT",
    "CREATE TABLE agentos_core_permission_outcomes (\
      session_id TEXT NOT NULL, request_id TEXT NOT NULL, \
      terminal_reason TEXT NOT NULL CHECK (terminal_reason IN ('accepted', 'already_resolved', 'prompt_cancelled', 'adapter_exited', 'session_deleted', 'vm_shutdown')), \
      terminal_at_ms INTEGER NOT NULL CHECK (terminal_at_ms >= 0), PRIMARY KEY (session_id, request_id)) STRICT",
    "CREATE INDEX agentos_core_permission_outcomes_by_age ON agentos_core_permission_outcomes (terminal_at_ms, session_id, request_id)",
    "CREATE INDEX agentos_core_sessions_by_activity ON agentos_core_sessions (updated_at_ms DESC, session_id)",
    "CREATE INDEX agentos_core_prompts_by_state ON agentos_core_prompts (state, updated_at_ms, session_id, prompt_id)",
    "CREATE UNIQUE INDEX agentos_core_prompts_one_active ON agentos_core_prompts (session_id) WHERE state = 'accepted'",
    "CREATE INDEX agentos_core_permission_records_by_prompt ON agentos_core_permission_records (session_id, prompt_id)",
];

const SESSION_MIGRATIONS: &[VmSqliteMigration] = &[VmSqliteMigration {
    version: 1,
    statements: SESSION_MIGRATION_1,
}];

#[derive(Clone)]
pub(crate) struct SessionStore {
    database: SharedVmSqliteDatabase,
    max_history_bytes: usize,
    max_history_events: usize,
    max_sessions_per_vm: usize,
    max_prompts_per_session: usize,
    max_prompts_per_vm: usize,
    max_pending_permissions_per_session: usize,
    max_pending_permissions_per_vm: usize,
    max_permission_outcomes_per_session: usize,
    max_permission_outcomes_per_vm: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredSession {
    pub session_id: String,
    pub agent: String,
    pub acp_session_id: Option<String>,
    pub state: String,
    pub state_json: String,
    pub cwd: String,
    pub creation_options_json: String,
    pub permission_policy: String,
    pub skip_os_instructions: bool,
    pub additional_instructions: Option<String>,
    pub additional_directories_json: String,
    pub env_json: String,
    pub mcp_servers_json: String,
    pub capabilities_json: Option<String>,
    pub agent_info_json: Option<String>,
    pub config_revision: i64,
    pub config_options_json: String,
    pub title: Option<String>,
    pub metadata_json: Option<String>,
    pub latest_sequence: i64,
    pub oldest_retained_sequence: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredSessionSummary {
    pub session_id: String,
    pub agent: String,
    pub state: String,
    pub state_json: String,
    pub cwd: String,
    pub additional_directories_json: String,
    pub latest_sequence: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub title: Option<String>,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredEvent {
    pub sequence: i64,
    pub occurred_at_ms: i64,
    pub event_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingRequestResolution {
    Accepted(StoredEvent),
    Terminal {
        reason: String,
        event: Option<StoredEvent>,
    },
    NotFound,
}

#[derive(Debug, Clone)]
pub(crate) struct HistoryResult {
    pub events: Vec<StoredEvent>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StoredPrompt {
    pub prompt_id: String,
    pub input_hash: Vec<u8>,
    pub state: String,
    pub result_json: Option<String>,
    pub error_json: Option<String>,
}

impl SessionStore {
    pub async fn open(database: SharedVmSqliteDatabase) -> Result<Self, VmSqliteError> {
        migrate_schema(
            database.as_ref(),
            "core",
            "agentos_core_schema_version",
            SESSION_MIGRATIONS,
        )
        .await?;
        let store = Self::from_database(database);
        store.reconcile_history_counters().await?;
        store.prune_over_limit_history().await?;
        Ok(store)
    }

    /// Attach to a VM database already migrated by extension bootstrap.
    pub fn from_database(database: SharedVmSqliteDatabase) -> Self {
        let limits = AcpLimits::default();
        Self {
            database,
            max_history_bytes: limits.max_session_history_bytes,
            max_history_events: limits.max_session_history_events,
            max_sessions_per_vm: limits.max_sessions_per_vm,
            max_prompts_per_session: limits.max_prompts_per_session,
            max_prompts_per_vm: limits.max_prompts_per_vm,
            max_pending_permissions_per_session: limits.max_pending_permissions_per_session,
            max_pending_permissions_per_vm: limits.max_pending_permissions_per_vm,
            max_permission_outcomes_per_session: limits.max_permission_outcomes_per_session,
            max_permission_outcomes_per_vm: limits.max_permission_outcomes_per_vm,
        }
    }

    pub fn with_limits(mut self, limits: &AcpLimits) -> Self {
        self.max_history_bytes = limits.max_session_history_bytes;
        self.max_history_events = limits.max_session_history_events;
        self.max_sessions_per_vm = limits.max_sessions_per_vm;
        self.max_prompts_per_session = limits.max_prompts_per_session;
        self.max_prompts_per_vm = limits.max_prompts_per_vm;
        self.max_pending_permissions_per_session = limits.max_pending_permissions_per_session;
        self.max_pending_permissions_per_vm = limits.max_pending_permissions_per_vm;
        self.max_permission_outcomes_per_session = limits.max_permission_outcomes_per_session;
        self.max_permission_outcomes_per_vm = limits.max_permission_outcomes_per_vm;
        self
    }

    /// Resolve turns that were durably accepted but lost their live adapter task
    /// when the VM stopped. Delivery is deliberately reported as uncertain; no
    /// prompt is replayed. A new prompt can be submitted explicitly afterwards.
    pub async fn reconcile_interrupted_turns(&self) -> Result<(), VmSqliteError> {
        let now = now_ms();
        let error = serde_json::json!({
            "code": "prompt_delivery_uncertain",
            "message": "the VM stopped before this prompt reached a terminal durable commit; AgentOS did not replay it",
            "retryable": true,
        });
        let error_json = serde_json::to_string(&error)
            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
        self.database
            .transaction(vec![
                SqlStatement::new(
                    "UPDATE agentos_core_permission_records SET state = 'terminal', terminal_reason = 'vm_shutdown', updated_at_ms = ? WHERE state = 'pending' AND EXISTS (SELECT 1 FROM agentos_core_prompts p WHERE p.session_id = agentos_core_permission_records.session_id AND p.prompt_id = agentos_core_permission_records.prompt_id AND p.state = 'accepted')",
                    vec![SqlValue::SqlInteger(now)],
                ),
                SqlStatement::new(
                    "UPDATE agentos_core_prompts SET state = 'failed', error_json = ?, updated_at_ms = ? WHERE state = 'accepted'",
                    vec![text(&error_json), SqlValue::SqlInteger(now)],
                ),
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET state = 'failed', updated_at_ms = ? WHERE state IN ('running', 'waiting')",
                    vec![SqlValue::SqlInteger(now)],
                ),
            ])
            .await?;
        Ok(())
    }

    pub async fn get(&self, session_id: &str) -> Result<Option<StoredSession>, VmSqliteError> {
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT s.session_id, s.agent, s.acp_session_id, s.state, s.state_prompt_id, s.state_started_at_ms, s.cwd, s.permission_policy, s.skip_os_instructions, s.additional_instructions, s.additional_directories_json, s.env_json, s.mcp_servers_json, s.capabilities_json, s.agent_info_json, s.config_revision, s.config_options_json, s.title, s.metadata_json, s.latest_sequence, s.oldest_retained_sequence, s.retained_event_count, s.retained_event_bytes, s.created_at_ms, s.updated_at_ms, p.error_json FROM agentos_core_sessions s LEFT JOIN agentos_core_prompts p ON p.session_id = s.session_id AND p.prompt_id = s.state_prompt_id WHERE s.session_id = ?",
                vec![SqlValue::SqlText(session_id.to_owned())],
            ))
            .await?;
        match result
            .rows
            .first()
            .map(|row| decode_session(row))
            .transpose()?
        {
            Some(mut session) => {
                self.hydrate_pending_state(&mut session).await?;
                Ok(Some(session))
            }
            None => Ok(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        &self,
        session_id: &str,
        agent: &str,
        acp_session_id: &str,
        cwd: &str,
        creation_options_json: &str,
        capabilities_json: Option<&str>,
        agent_info_json: Option<&str>,
        config_options_json: &str,
    ) -> Result<(), VmSqliteError> {
        let now = now_ms();
        let creation = parse_creation_options(creation_options_json)?;
        let limit = sqlite_limit(self.max_sessions_per_vm, "limits.acp.maxSessionsPerVm")?;
        let results = self.database
            .transaction(vec![SqlStatement::new(
                "INSERT INTO agentos_core_sessions (session_id, agent, acp_session_id, state, cwd, permission_policy, skip_os_instructions, additional_instructions, additional_directories_json, env_json, mcp_servers_json, capabilities_json, agent_info_json, config_options_json, created_at_ms, updated_at_ms) SELECT ?, ?, ?, 'idle', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ? WHERE (SELECT COUNT(*) FROM agentos_core_sessions) < ?",
                vec![
                    text(session_id),
                    text(agent),
                    text(acp_session_id),
                    text(cwd),
                    text(&creation.permission_policy),
                    SqlValue::SqlInteger(i64::from(creation.skip_os_instructions)),
                    optional_text(creation.additional_instructions.as_deref()),
                    text(&creation.additional_directories_json),
                    text(&creation.env_json),
                    text(&creation.mcp_servers_json),
                    optional_text(capabilities_json),
                    optional_text(agent_info_json),
                    text(config_options_json),
                    SqlValue::SqlInteger(now),
                    SqlValue::SqlInteger(now),
                    SqlValue::SqlInteger(limit),
                ],
            ), SqlStatement::plain("SELECT COUNT(*) FROM agentos_core_sessions")])
            .await?;
        if results.first().is_none_or(|result| result.changes != 1) {
            return Err(VmSqliteError::DurableCollectionLimit {
                code: "acp_sessions_limit",
                used: self.max_sessions_per_vm,
                limit: self.max_sessions_per_vm,
                setting: "limits.acp.maxSessionsPerVm",
            });
        }
        self.warn_collection_pressure(
            "sessions",
            count_result(results.get(1), "session count")?,
            self.max_sessions_per_vm,
            "limits.acp.maxSessionsPerVm",
        );
        Ok(())
    }

    pub async fn update_negotiated(
        &self,
        session_id: &str,
        acp_session_id: &str,
        capabilities_json: Option<&str>,
        agent_info_json: Option<&str>,
        config_options_json: &str,
    ) -> Result<(), VmSqliteError> {
        self.database
            .query(SqlStatement::new(
                "UPDATE agentos_core_sessions SET acp_session_id = ?, capabilities_json = ?, agent_info_json = ?, config_options_json = ?, updated_at_ms = ? WHERE session_id = ?",
                vec![
                    text(acp_session_id),
                    optional_text(capabilities_json),
                    optional_text(agent_info_json),
                    text(config_options_json),
                    SqlValue::SqlInteger(now_ms()),
                    text(session_id),
                ],
            ))
            .await?;
        Ok(())
    }

    pub async fn replace_config(
        &self,
        session_id: &str,
        config_options_json: &str,
    ) -> Result<i64, VmSqliteError> {
        let results = self
            .database
            .transaction(vec![
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET config_revision = config_revision + 1, config_options_json = ?, updated_at_ms = ? WHERE session_id = ?",
                    vec![
                        text(config_options_json),
                        SqlValue::SqlInteger(now_ms()),
                        text(session_id),
                    ],
                ),
                SqlStatement::new(
                    "SELECT config_revision FROM agentos_core_sessions WHERE session_id = ?",
                    vec![text(session_id)],
                ),
            ])
            .await?;
        let result = results.get(1).ok_or_else(|| {
            VmSqliteError::InvalidResult("config transaction returned no revision".to_owned())
        })?;
        match result.rows.first().and_then(|row| row.first()) {
            Some(SqlValue::SqlInteger(revision)) => Ok(*revision),
            value => Err(VmSqliteError::InvalidResult(format!(
                "config revision was invalid: {value:?}"
            ))),
        }
    }

    pub async fn list(
        &self,
        before: Option<(i64, String)>,
        limit: usize,
    ) -> Result<Vec<StoredSessionSummary>, VmSqliteError> {
        let mut params = Vec::new();
        let predicate = if let Some((updated_at, session_id)) = before {
            params.push(SqlValue::SqlInteger(updated_at));
            params.push(SqlValue::SqlInteger(updated_at));
            params.push(SqlValue::SqlText(session_id));
            "WHERE s.updated_at_ms < ? OR (s.updated_at_ms = ? AND s.session_id > ?)"
        } else {
            ""
        };
        params.push(SqlValue::SqlInteger(
            i64::try_from(limit).unwrap_or(i64::MAX),
        ));
        let result = self
            .database
            .query(SqlStatement::new(
                format!("SELECT s.session_id, s.agent, s.state, s.state_started_at_ms, s.cwd, s.additional_directories_json, s.latest_sequence, s.created_at_ms, s.updated_at_ms, s.title, s.metadata_json, p.error_json FROM agentos_core_sessions s LEFT JOIN agentos_core_prompts p ON p.session_id = s.session_id AND p.prompt_id = s.state_prompt_id {predicate} ORDER BY s.updated_at_ms DESC, s.session_id ASC LIMIT ?"),
                params,
            ))
            .await?;
        let mut sessions = result
            .rows
            .iter()
            .map(|row| decode_session_summary(row))
            .collect::<Result<Vec<_>, _>>()?;
        self.hydrate_pending_summaries(&mut sessions).await?;
        Ok(sessions)
    }

    pub async fn delete(&self, session_id: &str) -> Result<(), VmSqliteError> {
        let now = now_ms();
        self.database
            .transaction(vec![
                SqlStatement::new(
                    "INSERT OR REPLACE INTO agentos_core_permission_outcomes (session_id, request_id, terminal_reason, terminal_at_ms) SELECT session_id, request_id, CASE WHEN state = 'pending' THEN 'session_deleted' ELSE COALESCE(terminal_reason, 'already_resolved') END, ? FROM agentos_core_permission_records WHERE session_id = ?",
                    vec![SqlValue::SqlInteger(now), text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_permission_records WHERE session_id = ?",
                    vec![text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_events WHERE session_id = ?",
                    vec![text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_prompts WHERE session_id = ?",
                    vec![text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_sessions WHERE session_id = ?",
                    vec![text(session_id)],
                ),
            ])
            .await?;
        self.prune_permission_outcomes(session_id).await?;
        Ok(())
    }

    pub async fn prompt_by_idempotency_key(
        &self,
        session_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<StoredPrompt>, VmSqliteError> {
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT prompt_id, input_hash, state, result_json, error_json FROM agentos_core_prompts WHERE session_id = ? AND idempotency_key = ?",
                vec![text(session_id), text(idempotency_key)],
            ))
            .await?;
        result
            .rows
            .first()
            .map(|row| decode_prompt(row))
            .transpose()
    }

    pub async fn accept_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
        idempotency_key: Option<&str>,
        input_hash: Vec<u8>,
        user_updates: &[Value],
    ) -> Result<Vec<StoredEvent>, VmSqliteError> {
        let durable_events = user_updates
            .iter()
            .cloned()
            .map(session_update_event)
            .collect::<Vec<_>>();
        self.validate_history_batch(&durable_events)?;
        self.ensure_prompt_capacity(session_id).await?;
        let count = i64::try_from(durable_events.len()).map_err(|_| {
            VmSqliteError::InvalidResult("too many prompt input updates".to_owned())
        })?;
        let encoded_events = durable_events
            .iter()
            .map(encode_event)
            .collect::<Result<Vec<_>, _>>()?;
        let added_bytes = encoded_events
            .iter()
            .map(|event| event.payload_bytes)
            .sum::<i64>();
        let session_prompt_limit = sqlite_limit(
            self.max_prompts_per_session,
            "limits.acp.maxPromptsPerSession",
        )?;
        let vm_prompt_limit = sqlite_limit(self.max_prompts_per_vm, "limits.acp.maxPromptsPerVm")?;
        let now = now_ms();
        let mut statements = vec![
            SqlStatement::new(
                "UPDATE agentos_core_sessions SET state = 'running', state_prompt_id = ?, state_started_at_ms = ?, latest_sequence = latest_sequence + ?, retained_event_count = retained_event_count + ?, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND state IN ('idle', 'failed') AND latest_sequence <= ?",
                vec![
                    text(prompt_id),
                    SqlValue::SqlInteger(now),
                    SqlValue::SqlInteger(count),
                    SqlValue::SqlInteger(count),
                    SqlValue::SqlInteger(added_bytes),
                    SqlValue::SqlInteger(now),
                    text(session_id),
                    SqlValue::SqlInteger(MAX_SAFE_SEQUENCE - count),
                ],
            )
            .expect_changes(1),
            SqlStatement::new(
                "INSERT INTO agentos_core_prompts (session_id, prompt_id, idempotency_key, input_hash, state, first_input_sequence, created_at_ms, updated_at_ms) SELECT session_id, ?, ?, ?, 'accepted', CASE WHEN ? = 0 THEN NULL ELSE latest_sequence - ? + 1 END, ?, ? FROM agentos_core_sessions WHERE session_id = ? AND state = 'running' AND (SELECT COUNT(*) FROM agentos_core_prompts WHERE session_id = ?) < ? AND (SELECT COUNT(*) FROM agentos_core_prompts) < ?",
                vec![
                    text(prompt_id),
                    optional_text(idempotency_key),
                    SqlValue::SqlBlob(input_hash),
                    SqlValue::SqlInteger(count),
                    SqlValue::SqlInteger(count),
                    SqlValue::SqlInteger(now),
                    SqlValue::SqlInteger(now),
                    text(session_id),
                    text(session_id),
                    SqlValue::SqlInteger(session_prompt_limit),
                    SqlValue::SqlInteger(vm_prompt_limit),
                ],
            )
            .expect_changes(1),
        ];
        for (index, event) in encoded_events.iter().enumerate() {
            let offset = count - i64::try_from(index).unwrap_or(i64::MAX);
            statements.push(SqlStatement::new(
                "INSERT INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, latest_sequence - ? + 1, ?, 1, ?, ?, ?, ?, ?, ? FROM agentos_core_sessions WHERE session_id = ?",
                vec![
                    SqlValue::SqlInteger(offset),
                    SqlValue::SqlInteger(now),
                    text(&event.kind),
                    optional_text(event.correlation_id.as_deref()),
                    text(&event.payload_json),
                    SqlValue::SqlInteger(event.payload_bytes),
                    optional_text(event.outcome_status.as_deref()),
                    optional_text(event.terminal_reason.as_deref()),
                    text(session_id),
                ],
            ));
        }
        statements.push(history_usage_statement(session_id));
        statements.push(SqlStatement::new(
            "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? ORDER BY sequence DESC LIMIT ?",
            vec![text(session_id), SqlValue::SqlInteger(count)],
        ));
        let results = match self.database.transaction(statements).await {
            Ok(results) => results,
            Err(error @ VmSqliteError::UnexpectedChanges { .. }) => {
                self.ensure_prompt_capacity(session_id).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        self.warn_history_pressure(&results, session_id, count, added_bytes)?;
        if results.first().is_none_or(|result| result.changes != 1)
            || results.get(1).is_none_or(|result| result.changes != 1)
        {
            return Err(VmSqliteError::InvalidResult(format!(
                "session {session_id} is missing, busy, or exhausted its durable sequence range"
            )));
        }
        let mut events = decode_events(results.last().ok_or_else(|| {
            VmSqliteError::InvalidResult("prompt acceptance returned no event result".to_owned())
        })?)?;
        events.reverse();
        self.prune_history(session_id).await?;
        Ok(events)
    }

    pub async fn finish_prompt(
        &self,
        session_id: &str,
        prompt_id: &str,
        output_updates: &[Value],
        last_output_sequence: Option<i64>,
        result_json: Option<&str>,
        error_json: Option<&str>,
    ) -> Result<Vec<StoredEvent>, VmSqliteError> {
        let durable_events = output_updates
            .iter()
            .cloned()
            .map(session_update_event)
            .collect::<Vec<_>>();
        self.validate_history_batch(&durable_events)?;
        let count = i64::try_from(durable_events.len()).map_err(|_| {
            VmSqliteError::InvalidResult("too many prompt output updates".to_owned())
        })?;
        let now = now_ms();
        let (state, prompt_state) = if error_json.is_some() {
            ("failed", "failed")
        } else {
            ("idle", "completed")
        };
        let encoded_events = durable_events
            .iter()
            .map(encode_event)
            .collect::<Result<Vec<_>, _>>()?;
        let added_bytes = encoded_events
            .iter()
            .map(|event| event.payload_bytes)
            .sum::<i64>();
        let mut statements = vec![SqlStatement::new(
            "UPDATE agentos_core_sessions SET state = ?, state_prompt_id = CASE WHEN ? = 'idle' THEN NULL ELSE state_prompt_id END, state_started_at_ms = CASE WHEN ? = 'idle' THEN NULL ELSE state_started_at_ms END, latest_sequence = latest_sequence + ?, retained_event_count = retained_event_count + ?, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND state_prompt_id = ? AND latest_sequence <= ?",
            vec![
                text(state),
                text(state),
                text(state),
                SqlValue::SqlInteger(count),
                SqlValue::SqlInteger(count),
                SqlValue::SqlInteger(added_bytes),
                SqlValue::SqlInteger(now),
                text(session_id),
                text(prompt_id),
                SqlValue::SqlInteger(MAX_SAFE_SEQUENCE - count),
            ],
        )
        .expect_changes(1)];
        for (index, event) in encoded_events.iter().enumerate() {
            let offset = count - i64::try_from(index).unwrap_or(i64::MAX);
            statements.push(SqlStatement::new(
                "INSERT INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, latest_sequence - ? + 1, ?, 1, ?, ?, ?, ?, ?, ? FROM agentos_core_sessions WHERE session_id = ?",
                vec![
                    SqlValue::SqlInteger(offset),
                    SqlValue::SqlInteger(now),
                    text(&event.kind),
                    optional_text(event.correlation_id.as_deref()),
                    text(&event.payload_json),
                    SqlValue::SqlInteger(event.payload_bytes),
                    optional_text(event.outcome_status.as_deref()),
                    optional_text(event.terminal_reason.as_deref()),
                    text(session_id),
                ],
            ));
        }
        statements.push(SqlStatement::new(
            "UPDATE agentos_core_prompts SET state = ?, result_json = ?, error_json = ?, last_output_sequence = COALESCE(?, CASE WHEN ? = 0 THEN last_output_sequence ELSE (SELECT latest_sequence FROM agentos_core_sessions WHERE session_id = ?) END), updated_at_ms = ? WHERE session_id = ? AND prompt_id = ?",
            vec![
                text(prompt_state),
                optional_text(result_json),
                optional_text(error_json),
                last_output_sequence
                    .map(SqlValue::SqlInteger)
                    .unwrap_or(SqlValue::SqlNull),
                SqlValue::SqlInteger(count),
                text(session_id),
                SqlValue::SqlInteger(now),
                text(session_id),
                text(prompt_id),
            ],
        )
        .expect_changes(1));
        statements.push(SqlStatement::new(
            "UPDATE agentos_core_permission_records SET state = 'terminal', terminal_reason = 'prompt_cancelled', updated_at_ms = ? WHERE session_id = ? AND prompt_id = ? AND state = 'pending'",
            vec![SqlValue::SqlInteger(now), text(session_id), text(prompt_id)],
        ));
        statements.push(history_usage_statement(session_id));
        statements.push(SqlStatement::new(
            "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? ORDER BY sequence DESC LIMIT ?",
            vec![text(session_id), SqlValue::SqlInteger(count)],
        ));
        let results = self.database.transaction(statements).await?;
        self.warn_history_pressure(&results, session_id, count, added_bytes)?;
        let prompt_result_index = usize::try_from(count)
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| {
                VmSqliteError::InvalidResult("invalid prompt result index".to_owned())
            })?;
        if results.first().is_none_or(|result| result.changes != 1)
            || results
                .get(prompt_result_index)
                .is_none_or(|result| result.changes != 1)
        {
            return Err(VmSqliteError::InvalidResult(format!(
                "session or prompt {session_id}/{prompt_id} disappeared while finishing prompt"
            )));
        }
        let mut events = decode_events(results.last().ok_or_else(|| {
            VmSqliteError::InvalidResult("prompt completion returned no event result".to_owned())
        })?)?;
        events.reverse();
        self.prune_history(session_id).await?;
        self.prune_prompt_records(session_id, 0).await?;
        self.prune_permission_outcomes(session_id).await?;
        Ok(events)
    }

    pub async fn create_pending_request(
        &self,
        session_id: &str,
        prompt_id: &str,
        request_id: &str,
        request_kind: &str,
        request_json: &str,
    ) -> Result<StoredEvent, VmSqliteError> {
        let now = now_ms();
        let event = serde_json::json!({
            "type": "permission_request",
            "requestId": request_id,
            "request": serde_json::from_str::<Value>(request_json)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
        });
        self.validate_history_batch(std::slice::from_ref(&event))?;
        self.ensure_pending_capacity(session_id).await?;
        let encoded = encode_event(&event)?;
        let session_limit = sqlite_limit(
            self.max_pending_permissions_per_session,
            "limits.acp.maxPendingPermissionsPerSession",
        )?;
        let vm_limit = sqlite_limit(
            self.max_pending_permissions_per_vm,
            "limits.acp.maxPendingPermissionsPerVm",
        )?;
        let results = match self
            .database
            .transaction(vec![
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET latest_sequence = latest_sequence + 1, retained_event_count = retained_event_count + 1, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND state IN ('running', 'waiting') AND latest_sequence < 9007199254740991",
                    vec![SqlValue::SqlInteger(encoded.payload_bytes), SqlValue::SqlInteger(now), text(session_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "INSERT INTO agentos_core_permission_records (session_id, request_id, prompt_id, request_kind, state, request_json, created_at_ms, updated_at_ms) SELECT ?, ?, ?, ?, 'pending', ?, ?, ? WHERE EXISTS (SELECT 1 FROM agentos_core_prompts WHERE session_id = ? AND prompt_id = ? AND state = 'accepted') AND (SELECT COUNT(*) FROM agentos_core_permission_records WHERE session_id = ? AND state = 'pending') < ? AND (SELECT COUNT(*) FROM agentos_core_permission_records WHERE state = 'pending') < ?",
                    vec![
                        text(session_id),
                        text(request_id),
                        text(prompt_id),
                        text(request_kind),
                        text(request_json),
                        SqlValue::SqlInteger(now),
                        SqlValue::SqlInteger(now),
                        text(session_id),
                        text(prompt_id),
                        text(session_id),
                        SqlValue::SqlInteger(session_limit),
                        SqlValue::SqlInteger(vm_limit),
                    ],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "INSERT INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, latest_sequence, ?, 1, ?, ?, ?, ?, NULL, NULL FROM agentos_core_sessions WHERE session_id = ?",
                    vec![SqlValue::SqlInteger(now), text(&encoded.kind), optional_text(encoded.correlation_id.as_deref()), text(&encoded.payload_json), SqlValue::SqlInteger(encoded.payload_bytes), text(session_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET state = 'waiting', updated_at_ms = ? WHERE session_id = ? AND state IN ('running', 'waiting')",
                    vec![SqlValue::SqlInteger(now), text(session_id)],
                )
                .expect_changes(1),
                history_usage_statement(session_id),
                SqlStatement::new(
                    "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? AND sequence = (SELECT latest_sequence FROM agentos_core_sessions WHERE session_id = ?)",
                    vec![text(session_id), text(session_id)],
                ),
            ])
            .await
        {
            Ok(results) => results,
            Err(error @ VmSqliteError::UnexpectedChanges { .. }) => {
                self.ensure_pending_capacity(session_id).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        if results.first().is_none_or(|result| result.changes != 1)
            || results.get(1).is_none_or(|result| result.changes != 1)
            || results.get(2).is_none_or(|result| result.changes != 1)
            || results.get(3).is_none_or(|result| result.changes != 1)
        {
            return Err(VmSqliteError::InvalidResult(format!(
                "prompt {session_id}/{prompt_id} cannot accept pending request {request_id}"
            )));
        }
        let event = decode_events(results.get(5).ok_or_else(|| {
            VmSqliteError::InvalidResult("pending request returned no durable event".to_owned())
        })?)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            VmSqliteError::InvalidResult("pending request durable event was missing".to_owned())
        })?;
        self.warn_history_pressure(&results, session_id, 1, encoded.payload_bytes)?;
        self.prune_history(session_id).await?;
        Ok(event)
    }

    pub async fn respond_pending_request(
        &self,
        session_id: &str,
        prompt_id: &str,
        request_id: &str,
        response_json: &str,
    ) -> Result<PendingRequestResolution, VmSqliteError> {
        let now = now_ms();
        let event = serde_json::json!({
            "type": "permission_response",
            "requestId": request_id,
            "response": serde_json::from_str::<Value>(response_json)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
            "status": "accepted",
        });
        self.validate_history_batch(std::slice::from_ref(&event))?;
        let encoded = encode_event(&event)?;
        let results = match self
            .database
            .transaction(vec![
                SqlStatement::new(
                    "UPDATE agentos_core_permission_records SET state = 'responded', response_json = ?, terminal_reason = 'accepted', updated_at_ms = ? WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'pending'",
                    vec![
                        text(response_json),
                        SqlValue::SqlInteger(now),
                        text(session_id),
                        text(prompt_id),
                        text(request_id),
                    ],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET state = CASE WHEN EXISTS (SELECT 1 FROM agentos_core_permission_records pending WHERE pending.session_id = ? AND pending.state = 'pending') THEN 'waiting' ELSE 'running' END, latest_sequence = latest_sequence + 1, retained_event_count = retained_event_count + 1, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND state = 'waiting' AND latest_sequence < 9007199254740991 AND EXISTS (SELECT 1 FROM agentos_core_permission_records WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'responded' AND terminal_sequence IS NULL)",
                    vec![
                        text(session_id),
                        SqlValue::SqlInteger(encoded.payload_bytes),
                        SqlValue::SqlInteger(now),
                        text(session_id),
                        text(session_id),
                        text(prompt_id),
                        text(request_id),
                    ],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "UPDATE agentos_core_permission_records SET terminal_sequence = (SELECT latest_sequence FROM agentos_core_sessions WHERE session_id = ?) WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'responded' AND terminal_sequence IS NULL",
                    vec![text(session_id), text(session_id), text(prompt_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "INSERT OR IGNORE INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, terminal_sequence, ?, 1, ?, ?, ?, ?, ?, ? FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ? AND terminal_sequence IS NOT NULL",
                    vec![SqlValue::SqlInteger(now), text(&encoded.kind), optional_text(encoded.correlation_id.as_deref()), text(&encoded.payload_json), SqlValue::SqlInteger(encoded.payload_bytes), optional_text(encoded.outcome_status.as_deref()), optional_text(encoded.terminal_reason.as_deref()), text(session_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "SELECT state, terminal_reason, terminal_sequence FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?",
                    vec![text(session_id), text(request_id)],
                ),
                history_usage_statement(session_id),
                SqlStatement::new(
                    "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? AND sequence = (SELECT terminal_sequence FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?)",
                    vec![text(session_id), text(session_id), text(request_id)],
                ),
            ])
            .await
        {
            Ok(results) => results,
            Err(error @ VmSqliteError::UnexpectedChanges { .. }) => {
                if self.pending_record_is_actionable(session_id, request_id).await? {
                    return Err(error);
                }
                return self.pending_request_resolution(session_id, request_id).await;
            }
            Err(error) => return Err(error),
        };
        if results.first().is_some_and(|result| result.changes == 1) {
            let event = decode_events(results.get(6).ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission response returned no durable event result".to_owned(),
                )
            })?)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission response durable event was missing".to_owned(),
                )
            })?;
            self.warn_history_pressure(&results, session_id, 1, encoded.payload_bytes)?;
            self.prune_history(session_id).await?;
            self.prune_permission_outcomes(session_id).await?;
            return Ok(PendingRequestResolution::Accepted(event));
        }
        let resolution = decode_pending_resolution(results.get(4), results.get(6))?;
        if resolution == PendingRequestResolution::NotFound {
            return self
                .pending_request_resolution(session_id, request_id)
                .await;
        }
        Ok(resolution)
    }

    pub async fn terminate_pending_request(
        &self,
        session_id: &str,
        prompt_id: &str,
        request_id: &str,
        reason: &str,
    ) -> Result<PendingRequestResolution, VmSqliteError> {
        let now = now_ms();
        let response_json = serde_json::to_string(&serde_json::json!({
            "outcome": { "outcome": "cancelled" }
        }))
        .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
        let event = serde_json::json!({
            "type": "permission_response",
            "requestId": request_id,
            "response": serde_json::from_str::<Value>(&response_json)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
            "status": "not_pending",
            "reason": reason,
        });
        self.validate_history_batch(std::slice::from_ref(&event))?;
        let encoded = encode_event(&event)?;
        let results = match self
            .database.transaction(vec![
                SqlStatement::new(
                    "UPDATE agentos_core_permission_records SET state = 'terminal', response_json = ?, terminal_reason = ?, updated_at_ms = ? WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'pending'",
                    vec![text(&response_json), text(reason), SqlValue::SqlInteger(now), text(session_id), text(prompt_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "UPDATE agentos_core_sessions SET state = CASE WHEN EXISTS (SELECT 1 FROM agentos_core_permission_records pending WHERE pending.session_id = ? AND pending.state = 'pending') THEN 'waiting' ELSE 'running' END, latest_sequence = latest_sequence + 1, retained_event_count = retained_event_count + 1, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND state = 'waiting' AND latest_sequence < 9007199254740991 AND EXISTS (SELECT 1 FROM agentos_core_permission_records WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'terminal' AND terminal_sequence IS NULL)",
                    vec![text(session_id), SqlValue::SqlInteger(encoded.payload_bytes), SqlValue::SqlInteger(now), text(session_id), text(session_id), text(prompt_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "UPDATE agentos_core_permission_records SET terminal_sequence = (SELECT latest_sequence FROM agentos_core_sessions WHERE session_id = ?) WHERE session_id = ? AND prompt_id = ? AND request_id = ? AND state = 'terminal' AND terminal_sequence IS NULL",
                    vec![text(session_id), text(session_id), text(prompt_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "INSERT OR IGNORE INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, terminal_sequence, ?, 1, ?, ?, ?, ?, ?, ? FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ? AND terminal_sequence IS NOT NULL",
                    vec![SqlValue::SqlInteger(now), text(&encoded.kind), optional_text(encoded.correlation_id.as_deref()), text(&encoded.payload_json), SqlValue::SqlInteger(encoded.payload_bytes), optional_text(encoded.outcome_status.as_deref()), optional_text(encoded.terminal_reason.as_deref()), text(session_id), text(request_id)],
                )
                .expect_changes(1),
                SqlStatement::new(
                    "SELECT state, terminal_reason, terminal_sequence FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?",
                    vec![text(session_id), text(request_id)],
                ),
                history_usage_statement(session_id),
                SqlStatement::new(
                    "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? AND sequence = (SELECT terminal_sequence FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?)",
                    vec![text(session_id), text(session_id), text(request_id)],
                ),
            ])
            .await
        {
            Ok(results) => results,
            Err(error @ VmSqliteError::UnexpectedChanges { .. }) => {
                if self.pending_record_is_actionable(session_id, request_id).await? {
                    return Err(error);
                }
                return self.pending_request_resolution(session_id, request_id).await;
            }
            Err(error) => return Err(error),
        };
        if results.first().is_some_and(|result| result.changes == 1) {
            let event = decode_events(results.get(6).ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "terminal permission response returned no event result".to_owned(),
                )
            })?)?
            .into_iter()
            .next();
            self.warn_history_pressure(&results, session_id, 1, encoded.payload_bytes)?;
            self.prune_history(session_id).await?;
            self.prune_permission_outcomes(session_id).await?;
            return Ok(PendingRequestResolution::Terminal {
                reason: reason.to_owned(),
                event,
            });
        }
        let resolution = decode_pending_resolution(results.get(4), results.get(6))?;
        if resolution == PendingRequestResolution::NotFound {
            return self
                .pending_request_resolution(session_id, request_id)
                .await;
        }
        Ok(resolution)
    }

    pub async fn pending_request_resolution(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<PendingRequestResolution, VmSqliteError> {
        let result = self.database.query(SqlStatement::new(
            "SELECT state, terminal_reason, terminal_sequence FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?",
            vec![text(session_id), text(request_id)],
        )).await?;
        let resolution = decode_pending_resolution(Some(&result), None)?;
        if resolution != PendingRequestResolution::NotFound {
            return Ok(resolution);
        }
        let tombstone = self.database.query(SqlStatement::new(
            "SELECT terminal_reason FROM agentos_core_permission_outcomes WHERE session_id = ? AND request_id = ?",
            vec![text(session_id), text(request_id)],
        )).await?;
        match tombstone.rows.first() {
            Some(row) => Ok(PendingRequestResolution::Terminal {
                reason: required_text(row, 0, "terminal_reason")?,
                event: None,
            }),
            None => Ok(PendingRequestResolution::NotFound),
        }
    }

    async fn pending_record_is_actionable(
        &self,
        session_id: &str,
        request_id: &str,
    ) -> Result<bool, VmSqliteError> {
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT 1 FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ? AND state = 'pending'",
                vec![text(session_id), text(request_id)],
            ))
            .await?;
        Ok(!result.rows.is_empty())
    }

    async fn hydrate_pending_state(
        &self,
        session: &mut StoredSession,
    ) -> Result<(), VmSqliteError> {
        if session.state != "waiting" {
            return Ok(());
        }
        let result = self.database.query(SqlStatement::new(
            "SELECT request_kind, request_id, request_json, created_at_ms FROM agentos_core_permission_records WHERE session_id = ? AND state = 'pending' ORDER BY created_at_ms, request_id",
            vec![text(&session.session_id)],
        )).await?;
        let requests = result
            .rows
            .iter()
            .map(|row| {
                let request_json = required_text(row, 2, "request_json")?;
                let request = serde_json::from_str::<Value>(&request_json)
                    .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
                pending_request_state(required_text(row, 1, "request_id")?, request)
            })
            .collect::<Result<Vec<_>, VmSqliteError>>()?;
        let waiting_since = result
            .rows
            .first()
            .map(|row| required_integer(row, 3, "created_at_ms").and_then(timestamp))
            .transpose()?;
        session.state_json = serde_json::to_string(&serde_json::json!({
            "status": "waiting",
            "waitingSince": waiting_since,
            "requests": requests,
        }))
        .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
        Ok(())
    }

    async fn hydrate_pending_summaries(
        &self,
        sessions: &mut [StoredSessionSummary],
    ) -> Result<(), VmSqliteError> {
        if sessions.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat_n("?", sessions.len())
            .collect::<Vec<_>>()
            .join(",");
        let result = self
            .database
            .query(SqlStatement::new(
                format!("SELECT session_id, request_kind, request_id, request_json, created_at_ms FROM agentos_core_permission_records WHERE state = 'pending' AND session_id IN ({placeholders}) ORDER BY session_id, created_at_ms, request_id"),
                sessions.iter().map(|session| text(&session.session_id)).collect(),
            ))
            .await?;
        let mut pending: BTreeMap<String, Vec<(Value, i64)>> = BTreeMap::new();
        for row in &result.rows {
            let request_json = required_text(row, 3, "request_json")?;
            pending
                .entry(required_text(row, 0, "session_id")?)
                .or_default()
                .push((
                    pending_request_state(
                        required_text(row, 2, "request_id")?,
                        serde_json::from_str::<Value>(&request_json)
                            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
                    )?,
                    required_integer(row, 4, "created_at_ms")?,
                ));
        }
        for session in sessions {
            if session.state != "waiting" {
                continue;
            }
            let requests = pending.remove(&session.session_id).unwrap_or_default();
            let waiting_since = requests
                .first()
                .map(|(_, created_at)| timestamp(*created_at))
                .transpose()?;
            session.state_json = serde_json::to_string(&serde_json::json!({
                "status": "waiting",
                "waitingSince": waiting_since,
                "requests": requests.into_iter().map(|(request, _)| request).collect::<Vec<_>>(),
            }))
            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
        }
        Ok(())
    }

    async fn ensure_pending_capacity(&self, session_id: &str) -> Result<(), VmSqliteError> {
        let result = self.database.query(SqlStatement::new(
            "SELECT (SELECT COUNT(*) FROM agentos_core_permission_records WHERE state = 'pending' AND session_id = ?) AS session_count, (SELECT COUNT(*) FROM agentos_core_permission_records WHERE state = 'pending') AS vm_count",
            vec![text(session_id)],
        )).await?;
        let row = result.rows.first().ok_or_else(|| {
            VmSqliteError::InvalidResult(
                "pending permission count query returned no row".to_owned(),
            )
        })?;
        let session_count = usize_count(row, 0, "session pending permission count")?;
        let vm_count = usize_count(row, 1, "VM pending permission count")?;
        enforce_collection_limit(
            "acp_pending_permissions_per_session_limit",
            session_count,
            self.max_pending_permissions_per_session,
            "limits.acp.maxPendingPermissionsPerSession",
        )?;
        enforce_collection_limit(
            "acp_pending_permissions_per_vm_limit",
            vm_count,
            self.max_pending_permissions_per_vm,
            "limits.acp.maxPendingPermissionsPerVm",
        )?;
        self.warn_collection_pressure(
            "pending permissions for session",
            session_count + 1,
            self.max_pending_permissions_per_session,
            "limits.acp.maxPendingPermissionsPerSession",
        );
        self.warn_collection_pressure(
            "pending permissions for VM",
            vm_count + 1,
            self.max_pending_permissions_per_vm,
            "limits.acp.maxPendingPermissionsPerVm",
        );
        Ok(())
    }

    async fn ensure_prompt_capacity(&self, session_id: &str) -> Result<(), VmSqliteError> {
        self.prune_prompt_records(session_id, 1).await?;
        let result = self.database.query(SqlStatement::new(
            "SELECT (SELECT COUNT(*) FROM agentos_core_prompts WHERE session_id = ?) AS session_count, (SELECT COUNT(*) FROM agentos_core_prompts) AS vm_count",
            vec![text(session_id)],
        )).await?;
        let row = result.rows.first().ok_or_else(|| {
            VmSqliteError::InvalidResult("prompt count query returned no row".to_owned())
        })?;
        let session_count = usize_count(row, 0, "session prompt count")?;
        let vm_count = usize_count(row, 1, "VM prompt count")?;
        enforce_collection_limit(
            "acp_prompts_per_session_limit",
            session_count,
            self.max_prompts_per_session,
            "limits.acp.maxPromptsPerSession",
        )?;
        enforce_collection_limit(
            "acp_prompts_per_vm_limit",
            vm_count,
            self.max_prompts_per_vm,
            "limits.acp.maxPromptsPerVm",
        )?;
        self.warn_collection_pressure(
            "retained prompts for session",
            session_count + 1,
            self.max_prompts_per_session,
            "limits.acp.maxPromptsPerSession",
        );
        self.warn_collection_pressure(
            "retained prompts for VM",
            vm_count + 1,
            self.max_prompts_per_vm,
            "limits.acp.maxPromptsPerVm",
        );
        Ok(())
    }

    async fn prune_prompt_records(
        &self,
        session_id: &str,
        reserve: usize,
    ) -> Result<(), VmSqliteError> {
        const PRUNE_BATCH: i64 = 256;
        let session_limit = sqlite_limit(
            self.max_prompts_per_session.saturating_sub(reserve),
            "limits.acp.maxPromptsPerSession",
        )?;
        let vm_limit = sqlite_limit(
            self.max_prompts_per_vm.saturating_sub(reserve),
            "limits.acp.maxPromptsPerVm",
        )?;
        loop {
            let counts = self.database.query(SqlStatement::new(
                "SELECT (SELECT COUNT(*) FROM agentos_core_prompts WHERE session_id = ?) AS session_count, (SELECT COUNT(*) FROM agentos_core_prompts) AS vm_count",
                vec![text(session_id)],
            )).await?;
            let row = counts.rows.first().ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "prompt pruning count query returned no row".to_owned(),
                )
            })?;
            let session_count = required_integer(row, 0, "session prompt count")?;
            let vm_count = required_integer(row, 1, "VM prompt count")?;
            if session_count <= session_limit && vm_count <= vm_limit {
                return Ok(());
            }
            let scope_session = session_count > session_limit;
            let overflow = if scope_session {
                session_count - session_limit
            } else {
                vm_count - vm_limit
            };
            let delete_limit = overflow.min(PRUNE_BATCH);
            let (predicate, params) = if scope_session {
                (
                    "p.session_id = ? AND",
                    vec![text(session_id), SqlValue::SqlInteger(delete_limit)],
                )
            } else {
                ("", vec![SqlValue::SqlInteger(delete_limit)])
            };
            let result = self.database.query(SqlStatement::new(
                format!("DELETE FROM agentos_core_prompts WHERE rowid IN (SELECT p.rowid FROM agentos_core_prompts p WHERE {predicate} p.state <> 'accepted' AND NOT EXISTS (SELECT 1 FROM agentos_core_sessions s WHERE s.session_id = p.session_id AND s.state_prompt_id = p.prompt_id) AND NOT EXISTS (SELECT 1 FROM agentos_core_permission_records r WHERE r.session_id = p.session_id AND r.prompt_id = p.prompt_id) ORDER BY p.updated_at_ms, p.session_id, p.prompt_id LIMIT ?)"),
                params,
            )).await?;
            if result.changes == 0 {
                return Ok(());
            }
        }
    }

    async fn prune_permission_outcomes(&self, session_id: &str) -> Result<(), VmSqliteError> {
        let now = now_ms();
        let per_session = sqlite_limit(
            self.max_permission_outcomes_per_session,
            "limits.acp.maxPermissionOutcomesPerSession",
        )?;
        let per_vm = sqlite_limit(
            self.max_permission_outcomes_per_vm,
            "limits.acp.maxPermissionOutcomesPerVm",
        )?;
        self.database
            .transaction(vec![
                SqlStatement::new(
                    "INSERT OR REPLACE INTO agentos_core_permission_outcomes (session_id, request_id, terminal_reason, terminal_at_ms) SELECT session_id, request_id, COALESCE(terminal_reason, 'already_resolved'), ? FROM agentos_core_permission_records WHERE state <> 'pending' AND session_id = ?",
                    vec![SqlValue::SqlInteger(now), text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_permission_records WHERE state <> 'pending' AND session_id = ?",
                    vec![text(session_id)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_permission_outcomes WHERE session_id = ? AND request_id IN (SELECT request_id FROM agentos_core_permission_outcomes WHERE session_id = ? ORDER BY terminal_at_ms DESC, request_id DESC LIMIT -1 OFFSET ?)",
                    vec![text(session_id), text(session_id), SqlValue::SqlInteger(per_session)],
                ),
                SqlStatement::new(
                    "DELETE FROM agentos_core_permission_outcomes WHERE (terminal_at_ms, session_id, request_id) IN (SELECT terminal_at_ms, session_id, request_id FROM agentos_core_permission_outcomes ORDER BY terminal_at_ms DESC, session_id DESC, request_id DESC LIMIT -1 OFFSET ?)",
                    vec![SqlValue::SqlInteger(per_vm)],
                ),
            ])
            .await?;
        let counts = self.database.query(SqlStatement::new(
            "SELECT (SELECT COUNT(*) FROM agentos_core_permission_outcomes WHERE session_id = ?) AS session_count, (SELECT COUNT(*) FROM agentos_core_permission_outcomes) AS vm_count",
            vec![text(session_id)],
        )).await?;
        if let Some(row) = counts.rows.first() {
            self.warn_collection_pressure(
                "permission outcomes for session",
                usize_count(row, 0, "session outcome count")?,
                self.max_permission_outcomes_per_session,
                "limits.acp.maxPermissionOutcomesPerSession",
            );
            self.warn_collection_pressure(
                "permission outcomes for VM",
                usize_count(row, 1, "VM outcome count")?,
                self.max_permission_outcomes_per_vm,
                "limits.acp.maxPermissionOutcomesPerVm",
            );
        }
        Ok(())
    }

    fn warn_collection_pressure(&self, label: &str, used: usize, limit: usize, setting: &str) {
        if used >= limit.saturating_mul(4) / 5 {
            tracing::warn!(
                label,
                used,
                limit,
                setting,
                "durable SQLite collection is near its configured limit"
            );
        }
    }

    pub async fn append_updates(
        &self,
        session_id: &str,
        acp_protocol_version: i64,
        updates: &[Value],
    ) -> Result<Vec<StoredEvent>, VmSqliteError> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let durable_events = updates
            .iter()
            .cloned()
            .map(session_update_event)
            .collect::<Vec<_>>();
        self.validate_history_batch(&durable_events)?;
        let count = i64::try_from(durable_events.len()).map_err(|_| {
            VmSqliteError::InvalidResult("too many session updates in one commit".to_owned())
        })?;
        let encoded_events = durable_events
            .iter()
            .map(encode_event)
            .collect::<Result<Vec<_>, _>>()?;
        let added_bytes = encoded_events
            .iter()
            .map(|event| event.payload_bytes)
            .sum::<i64>();
        let now = now_ms();
        let mut statements = vec![SqlStatement::new(
            "UPDATE agentos_core_sessions SET latest_sequence = latest_sequence + ?, retained_event_count = retained_event_count + ?, retained_event_bytes = retained_event_bytes + ?, updated_at_ms = ? WHERE session_id = ? AND latest_sequence <= ?",
            vec![
                SqlValue::SqlInteger(count),
                SqlValue::SqlInteger(count),
                SqlValue::SqlInteger(added_bytes),
                SqlValue::SqlInteger(now),
                text(session_id),
                SqlValue::SqlInteger(MAX_SAFE_SEQUENCE - count),
            ],
        )
        .expect_changes(1)];
        for (index, event) in encoded_events.iter().enumerate() {
            let offset = count - i64::try_from(index).unwrap_or(i64::MAX);
            statements.push(SqlStatement::new(
                "INSERT INTO agentos_core_events (session_id, sequence, occurred_at_ms, acp_protocol_version, event_kind, correlation_id, payload_json, payload_bytes, outcome_status, terminal_reason) SELECT session_id, latest_sequence - ? + 1, ?, ?, ?, ?, ?, ?, ?, ? FROM agentos_core_sessions WHERE session_id = ?",
                vec![
                    SqlValue::SqlInteger(offset),
                    SqlValue::SqlInteger(now),
                    SqlValue::SqlInteger(acp_protocol_version),
                    text(&event.kind),
                    optional_text(event.correlation_id.as_deref()),
                    text(&event.payload_json),
                    SqlValue::SqlInteger(event.payload_bytes),
                    optional_text(event.outcome_status.as_deref()),
                    optional_text(event.terminal_reason.as_deref()),
                    text(session_id),
                ],
            ));
        }
        statements.push(history_usage_statement(session_id));
        statements.push(SqlStatement::new(
            "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? ORDER BY sequence DESC LIMIT ?",
            vec![text(session_id), SqlValue::SqlInteger(count)],
        ));
        let results = self.database.transaction(statements).await?;
        self.warn_history_pressure(&results, session_id, count, added_bytes)?;
        if results.first().is_none_or(|result| result.changes != 1) {
            return Err(VmSqliteError::InvalidResult(format!(
                "session {session_id} disappeared while appending history"
            )));
        }
        let mut events = decode_events(results.last().ok_or_else(|| {
            VmSqliteError::InvalidResult("append transaction returned no result".to_owned())
        })?)?;
        events.reverse();
        self.prune_history(session_id).await?;
        Ok(events)
    }

    async fn prune_history(&self, session_id: &str) -> Result<(), VmSqliteError> {
        const PRUNE_BATCH: i64 = 256;
        let max_events = sqlite_limit(
            self.max_history_events,
            "limits.acp.maxSessionHistoryEvents",
        )?;
        let max_bytes = sqlite_limit(self.max_history_bytes, "limits.acp.maxSessionHistoryBytes")?;
        loop {
            let usage = self
                .database
                .query(history_usage_statement(session_id))
                .await?;
            let Some(row) = usage.rows.first() else {
                // A concurrent session deletion has already removed the history that
                // this best-effort retention pass was going to prune.
                return Ok(());
            };
            let event_count = required_integer(row, 0, "retained_event_count")?;
            let event_bytes = required_integer(row, 1, "retained_event_bytes")?;
            if event_count <= max_events && event_bytes <= max_bytes {
                return Ok(());
            }
            let oldest = self
                .database
                .query(SqlStatement::new(
                    "SELECT sequence, payload_bytes FROM agentos_core_events WHERE session_id = ? ORDER BY sequence ASC LIMIT ?",
                    vec![text(session_id), SqlValue::SqlInteger(PRUNE_BATCH)],
                ))
                .await?;
            if oldest.rows.is_empty() {
                tracing::error!(
                    session_id,
                    event_count,
                    event_bytes,
                    "durable history counters disagree with event rows; reconciling"
                );
                self.reconcile_history_counters().await?;
                continue;
            }
            let mut removed_count = 0i64;
            let mut removed_bytes = 0i64;
            let mut cutoff = 0i64;
            for row in &oldest.rows {
                cutoff = required_integer(row, 0, "sequence")?;
                removed_count += 1;
                removed_bytes =
                    removed_bytes.saturating_add(required_integer(row, 1, "payload_bytes")?);
                if event_count - removed_count <= max_events
                    && event_bytes - removed_bytes <= max_bytes
                {
                    break;
                }
            }
            let results = self
                .database
                .transaction(vec![
                    SqlStatement::new(
                        "UPDATE agentos_core_sessions SET retained_event_count = retained_event_count - ?, retained_event_bytes = retained_event_bytes - ? WHERE session_id = ? AND retained_event_count >= ? AND retained_event_bytes >= ?",
                        vec![SqlValue::SqlInteger(removed_count), SqlValue::SqlInteger(removed_bytes), text(session_id), SqlValue::SqlInteger(removed_count), SqlValue::SqlInteger(removed_bytes)],
                    )
                    .expect_changes(1),
                    SqlStatement::new(
                        "DELETE FROM agentos_core_events WHERE session_id = ? AND sequence <= ?",
                        vec![text(session_id), SqlValue::SqlInteger(cutoff)],
                    )
                    .expect_changes(removed_count),
                    SqlStatement::new(
                        "UPDATE agentos_core_sessions SET oldest_retained_sequence = COALESCE((SELECT MIN(sequence) FROM agentos_core_events WHERE session_id = ?), MIN(latest_sequence + 1, 9007199254740991)) WHERE session_id = ?",
                        vec![text(session_id), text(session_id)],
                    )
                    .expect_changes(1),
                ])
                .await;
            if let Err(error) = results {
                tracing::error!(session_id, %error, "durable history pruning detected counter corruption; reconciling");
                self.reconcile_history_counters().await?;
            }
        }
    }

    /// Apply this store instance's configured retention budget and then return
    /// fresh cursor bounds. This is required after a VM wake because database
    /// bootstrap uses defaults while the request-scoped store carries the
    /// actual VM limits.
    pub async fn enforce_history_retention(
        &self,
        session_id: &str,
    ) -> Result<Option<StoredSession>, VmSqliteError> {
        self.prune_history(session_id).await?;
        self.get(session_id).await
    }

    async fn prune_over_limit_history(&self) -> Result<(), VmSqliteError> {
        const SESSION_BATCH: i64 = 256;
        let max_events = sqlite_limit(
            self.max_history_events,
            "limits.acp.maxSessionHistoryEvents",
        )?;
        let max_bytes = sqlite_limit(self.max_history_bytes, "limits.acp.maxSessionHistoryBytes")?;
        loop {
            let result = self
                .database
                .query(SqlStatement::new(
                    "SELECT session_id FROM agentos_core_sessions WHERE retained_event_count > ? OR retained_event_bytes > ? ORDER BY session_id LIMIT ?",
                    vec![
                        SqlValue::SqlInteger(max_events),
                        SqlValue::SqlInteger(max_bytes),
                        SqlValue::SqlInteger(SESSION_BATCH),
                    ],
                ))
                .await?;
            if result.rows.is_empty() {
                return Ok(());
            }
            for row in &result.rows {
                self.prune_history(&required_text(row, 0, "session_id")?)
                    .await?;
            }
        }
    }

    async fn reconcile_history_counters(&self) -> Result<(), VmSqliteError> {
        let mismatches = self.database.query(SqlStatement::plain(
            "SELECT s.session_id FROM agentos_core_sessions s WHERE s.retained_event_count <> (SELECT COUNT(*) FROM agentos_core_events e WHERE e.session_id = s.session_id) OR s.retained_event_bytes <> COALESCE((SELECT SUM(e.payload_bytes) FROM agentos_core_events e WHERE e.session_id = s.session_id), 0) OR s.oldest_retained_sequence <> COALESCE((SELECT MIN(e.sequence) FROM agentos_core_events e WHERE e.session_id = s.session_id), MIN(s.latest_sequence + 1, 9007199254740991)) LIMIT 1",
        )).await?;
        if mismatches.rows.is_empty() {
            return Ok(());
        }
        tracing::warn!(
            "reconciling durable history counters from event rows during schema bootstrap"
        );
        self.database.transaction(vec![SqlStatement::plain(
            "UPDATE agentos_core_sessions SET retained_event_count = (SELECT COUNT(*) FROM agentos_core_events e WHERE e.session_id = agentos_core_sessions.session_id), retained_event_bytes = COALESCE((SELECT SUM(e.payload_bytes) FROM agentos_core_events e WHERE e.session_id = agentos_core_sessions.session_id), 0), oldest_retained_sequence = COALESCE((SELECT MIN(e.sequence) FROM agentos_core_events e WHERE e.session_id = agentos_core_sessions.session_id), MIN(latest_sequence + 1, 9007199254740991))",
        )]).await?;
        Ok(())
    }

    fn validate_history_batch(&self, updates: &[Value]) -> Result<(), VmSqliteError> {
        if updates.len() > self.max_history_events {
            return Err(VmSqliteError::HistoryEventBatchTooLarge {
                used: updates.len(),
                limit: self.max_history_events,
            });
        }
        let bytes = updates.iter().try_fold(0usize, |total, update| {
            let update_bytes =
                usize::try_from(encode_event(update)?.payload_bytes).map_err(|_| {
                    VmSqliteError::InvalidResult("event payload length is negative".to_owned())
                })?;
            total
                .checked_add(update_bytes)
                .ok_or(VmSqliteError::HistoryByteBatchTooLarge {
                    used: usize::MAX,
                    limit: self.max_history_bytes,
                })
        })?;
        if bytes > self.max_history_bytes {
            return Err(VmSqliteError::HistoryByteBatchTooLarge {
                used: bytes,
                limit: self.max_history_bytes,
            });
        }
        Ok(())
    }

    fn warn_history_pressure(
        &self,
        results: &[QueryResult],
        session_id: &str,
        added_events: i64,
        added_bytes: i64,
    ) -> Result<(), VmSqliteError> {
        let Some(usage) = results.iter().find(|result| {
            result.columns
                == [
                    "agentos_history_event_count".to_owned(),
                    "agentos_history_bytes".to_owned(),
                ]
        }) else {
            return Err(VmSqliteError::InvalidResult(
                "history retention transaction omitted usage result".to_owned(),
            ));
        };
        let row = usage.rows.first().ok_or_else(|| {
            VmSqliteError::InvalidResult("history usage result had no row".to_owned())
        })?;
        let total_events = sql_integer(row.first(), "history event count")?;
        let total_bytes = sql_integer(row.get(1), "history bytes")?;
        warn_history_crossing(
            session_id,
            "events",
            total_events,
            added_events,
            i64::try_from(self.max_history_events).unwrap_or(i64::MAX),
            "limits.acp.maxSessionHistoryEvents",
        );
        warn_history_crossing(
            session_id,
            "bytes",
            total_bytes,
            added_bytes,
            i64::try_from(self.max_history_bytes).unwrap_or(i64::MAX),
            "limits.acp.maxSessionHistoryBytes",
        );
        Ok(())
    }

    pub async fn read_history(
        &self,
        session: &StoredSession,
        before: Option<i64>,
        after: Option<i64>,
        limit: usize,
    ) -> Result<HistoryResult, VmSqliteError> {
        let fetch_limit = limit.saturating_add(1);
        let (sql, params, reverse) = if let Some(before) = before {
            (
                "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? AND sequence < ? ORDER BY sequence DESC LIMIT ?",
                vec![text(&session.session_id), SqlValue::SqlInteger(before), SqlValue::SqlInteger(fetch_limit as i64)],
                true,
            )
        } else if let Some(after) = after {
            (
                "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? AND sequence > ? ORDER BY sequence ASC LIMIT ?",
                vec![text(&session.session_id), SqlValue::SqlInteger(after), SqlValue::SqlInteger(fetch_limit as i64)],
                false,
            )
        } else {
            (
                "SELECT sequence, occurred_at_ms, event_kind, correlation_id, payload_json, outcome_status, terminal_reason FROM agentos_core_events WHERE session_id = ? ORDER BY sequence DESC LIMIT ?",
                vec![text(&session.session_id), SqlValue::SqlInteger(fetch_limit as i64)],
                true,
            )
        };
        let result = self.database.query(SqlStatement::new(sql, params)).await?;
        let had_extra = result.rows.len() > limit;
        let bounded = QueryResult {
            rows: result.rows.into_iter().take(limit).collect(),
            ..result
        };
        let mut events = decode_events(&bounded)?;
        if reverse {
            events.reverse();
        }
        let first = events.first().map(|event| event.sequence);
        let last = events.last().map(|event| event.sequence);
        let requested_before = before;
        let requested_after = after;
        Ok(HistoryResult {
            has_more_before: if reverse {
                had_extra
                    || (events.is_empty()
                        && requested_before
                            .is_some_and(|cursor| cursor > session.oldest_retained_sequence))
            } else {
                first.is_some_and(|value| value > session.oldest_retained_sequence)
                    || (events.is_empty()
                        && requested_after
                            .is_some_and(|cursor| cursor > session.oldest_retained_sequence))
            },
            has_more_after: if reverse {
                last.is_some_and(|value| value < session.latest_sequence)
                    || (events.is_empty()
                        && requested_before.is_some_and(|cursor| cursor <= session.latest_sequence))
            } else {
                had_extra
                    || (events.is_empty()
                        && requested_after.is_some_and(|cursor| cursor < session.latest_sequence))
            },
            events,
        })
    }
}

fn sql_integer(value: Option<&SqlValue>, label: &str) -> Result<i64, VmSqliteError> {
    match value {
        Some(SqlValue::SqlInteger(value)) => Ok(*value),
        _ => Err(VmSqliteError::InvalidResult(format!(
            "{label} must be an integer"
        ))),
    }
}

fn sqlite_limit(limit: usize, setting: &str) -> Result<i64, VmSqliteError> {
    i64::try_from(limit)
        .map_err(|_| VmSqliteError::InvalidResult(format!("{setting} is too large for SQLite")))
}

fn usize_count(row: &[SqlValue], index: usize, label: &str) -> Result<usize, VmSqliteError> {
    usize::try_from(required_integer(row, index, label)?)
        .map_err(|_| VmSqliteError::InvalidResult(format!("{label} was outside the usize range")))
}

fn count_result(result: Option<&QueryResult>, label: &str) -> Result<usize, VmSqliteError> {
    let row = result
        .and_then(|result| result.rows.first())
        .ok_or_else(|| VmSqliteError::InvalidResult(format!("{label} query returned no row")))?;
    usize_count(row, 0, label)
}

fn enforce_collection_limit(
    code: &'static str,
    used: usize,
    limit: usize,
    setting: &'static str,
) -> Result<(), VmSqliteError> {
    if used >= limit {
        return Err(VmSqliteError::DurableCollectionLimit {
            code,
            used,
            limit,
            setting,
        });
    }
    Ok(())
}

fn history_usage_statement(session_id: &str) -> SqlStatement {
    SqlStatement::new(
        "SELECT retained_event_count AS agentos_history_event_count, retained_event_bytes AS agentos_history_bytes FROM agentos_core_sessions WHERE session_id = ?",
        vec![text(session_id)],
    )
}

#[cfg(test)]
fn permission_outcome_prune_statement(limit: i64) -> SqlStatement {
    SqlStatement::new(
        "DELETE FROM agentos_core_permission_outcomes WHERE (terminal_at_ms, session_id, request_id) IN (SELECT terminal_at_ms, session_id, request_id FROM agentos_core_permission_outcomes ORDER BY terminal_at_ms DESC, session_id DESC, request_id DESC LIMIT -1 OFFSET ?)",
        vec![SqlValue::SqlInteger(limit)],
    )
}

fn warn_history_crossing(
    session_id: &str,
    resource: &str,
    total: i64,
    added: i64,
    limit: i64,
    config_path: &str,
) {
    let threshold = limit.saturating_mul(4) / 5;
    if total >= threshold && total.saturating_sub(added) < threshold {
        tracing::warn!(
            session_id,
            resource,
            used = total,
            limit,
            config_path,
            "durable ACP history is near its configured retention limit"
        );
    }
}

pub(crate) fn timestamp(ms: i64) -> Result<String, VmSqliteError> {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|timestamp| timestamp.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| VmSqliteError::InvalidResult(format!("invalid timestamp {ms}")))
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn text(value: &str) -> SqlValue {
    SqlValue::SqlText(value.to_owned())
}

fn optional_text(value: Option<&str>) -> SqlValue {
    value.map(text).unwrap_or(SqlValue::SqlNull)
}

fn validate_update(update: &Value) -> Result<&str, VmSqliteError> {
    let update_type = update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            VmSqliteError::InvalidResult("ACP update is missing sessionUpdate".to_owned())
        })?;
    serde_json::from_value::<AcpSessionUpdate>(update.clone()).map_err(|error| {
        VmSqliteError::InvalidResult(format!(
            "invalid negotiated ACP session update {update_type}: {error}"
        ))
    })?;
    Ok(update_type)
}

fn pending_request_state(request_id: String, request: Value) -> Result<Value, VmSqliteError> {
    let mut request = request.as_object().cloned().ok_or_else(|| {
        VmSqliteError::InvalidResult("pending permission request must be an object".to_owned())
    })?;
    request.remove("sessionId");
    request.insert("requestId".to_owned(), Value::String(request_id));
    Ok(Value::Object(request))
}

fn session_update_event(update: Value) -> Value {
    serde_json::json!({ "type": "session_update", "update": update })
}

fn validate_event(event: &Value) -> Result<&str, VmSqliteError> {
    let validate_request_id = || {
        let request_id = event
            .get("requestId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "durable permission event is missing requestId".to_owned(),
                )
            })?;
        if request_id.is_empty() || request_id.len() > 256 {
            return Err(VmSqliteError::InvalidResult(
                "durable permission event requestId must contain 1..=256 bytes".to_owned(),
            ));
        }
        Ok(())
    };
    match event.get("type").and_then(Value::as_str) {
        Some("session_update") => {
            validate_update(event.get("update").ok_or_else(|| {
                VmSqliteError::InvalidResult("session_update event is missing update".to_owned())
            })?)?;
            Ok("session_update")
        }
        Some("permission_request") => {
            validate_request_id()?;
            let request = event.get("request").ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission_request event is missing request".to_owned(),
                )
            })?;
            serde_json::from_value::<AcpRequestPermissionRequest>(request.clone()).map_err(
                |error| {
                    VmSqliteError::InvalidResult(format!(
                        "invalid ACP permission request event payload: {error}"
                    ))
                },
            )?;
            Ok("permission_request")
        }
        Some("permission_response") => {
            validate_request_id()?;
            let response = event.get("response").ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission_response event is missing response".to_owned(),
                )
            })?;
            serde_json::from_value::<AcpRequestPermissionResponse>(response.clone()).map_err(
                |error| {
                    VmSqliteError::InvalidResult(format!(
                        "invalid ACP permission response event payload: {error}"
                    ))
                },
            )?;
            match (
                event.get("status").and_then(Value::as_str),
                event.get("reason").and_then(Value::as_str),
            ) {
                (Some("accepted"), None) => {}
                (
                    Some("not_pending"),
                    Some(
                        "already_resolved" | "prompt_cancelled" | "adapter_exited"
                        | "session_deleted" | "vm_shutdown",
                    ),
                ) => {}
                _ => {
                    return Err(VmSqliteError::InvalidResult(
                        "permission_response event status/reason combination is invalid".to_owned(),
                    ));
                }
            }
            Ok("permission_response")
        }
        kind => Err(VmSqliteError::InvalidResult(format!(
            "unknown durable session event type {kind:?}"
        ))),
    }
}

struct EncodedEvent {
    kind: String,
    correlation_id: Option<String>,
    payload_json: String,
    payload_bytes: i64,
    outcome_status: Option<String>,
    terminal_reason: Option<String>,
}

fn encode_event(event: &Value) -> Result<EncodedEvent, VmSqliteError> {
    let kind = validate_event(event)?;
    let (payload, correlation_id, outcome_status, terminal_reason) = match kind {
        "session_update" => (
            event.get("update").cloned().ok_or_else(|| {
                VmSqliteError::InvalidResult("session_update event is missing update".to_owned())
            })?,
            None,
            None,
            None,
        ),
        "permission_request" => (
            event.get("request").cloned().ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission_request event is missing request".to_owned(),
                )
            })?,
            event
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            None,
            None,
        ),
        "permission_response" => (
            event.get("response").cloned().ok_or_else(|| {
                VmSqliteError::InvalidResult(
                    "permission_response event is missing response".to_owned(),
                )
            })?,
            event
                .get("requestId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            event
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned),
            event
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ),
        _ => unreachable!("validate_event returned a known kind"),
    };
    let payload_json = serde_json::to_string(&payload)
        .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
    let payload_bytes = i64::try_from(payload_json.len()).map_err(|_| {
        VmSqliteError::InvalidResult("event payload exceeds SQLite integer range".to_owned())
    })?;
    Ok(EncodedEvent {
        kind: kind.to_owned(),
        correlation_id,
        payload_json,
        payload_bytes,
        outcome_status,
        terminal_reason,
    })
}

struct StoredCreationOptions {
    permission_policy: String,
    skip_os_instructions: bool,
    additional_instructions: Option<String>,
    additional_directories_json: String,
    env_json: String,
    mcp_servers_json: String,
}

fn parse_creation_options(value: &str) -> Result<StoredCreationOptions, VmSqliteError> {
    let value: Value = serde_json::from_str(value)
        .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
    let object = value.as_object().ok_or_else(|| {
        VmSqliteError::InvalidResult("creation options must be a JSON object".to_owned())
    })?;
    let permission_policy = object
        .get("permissionPolicy")
        .and_then(Value::as_str)
        .unwrap_or("allow_all")
        .to_owned();
    if !matches!(
        permission_policy.as_str(),
        "allow_all" | "reject_all" | "ask"
    ) {
        return Err(VmSqliteError::InvalidResult(format!(
            "invalid permission policy {permission_policy:?}"
        )));
    }
    let skip_os_instructions = object
        .get("skipOsInstructions")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let additional_instructions = object
        .get("additionalInstructions")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let additional_directories_json = canonical_json(
        object
            .get("additionalDirectories")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "additionalDirectories",
        "array",
    )?;
    let env_json = canonical_json(
        object
            .get("env")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
        "env",
        "object",
    )?;
    let mcp_servers_json = canonical_json(
        object
            .get("mcpServers")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
        "mcpServers",
        "array",
    )?;
    Ok(StoredCreationOptions {
        permission_policy,
        skip_os_instructions,
        additional_instructions,
        additional_directories_json,
        env_json,
        mcp_servers_json,
    })
}

fn canonical_json(value: Value, field: &str, expected: &str) -> Result<String, VmSqliteError> {
    let valid = matches!(
        (expected, &value),
        ("array", Value::Array(_)) | ("object", Value::Object(_))
    );
    if !valid {
        return Err(VmSqliteError::InvalidResult(format!(
            "creation option {field} must be a JSON {expected}"
        )));
    }
    serde_json::to_string(&value).map_err(|error| VmSqliteError::InvalidResult(error.to_string()))
}

fn creation_options_json(
    cwd: &str,
    permission_policy: &str,
    skip_os_instructions: bool,
    additional_instructions: Option<&str>,
    additional_directories_json: &str,
    env_json: &str,
    mcp_servers_json: &str,
) -> Result<String, VmSqliteError> {
    serde_json::to_string(&serde_json::json!({
        "formatVersion": 1,
        "cwd": cwd,
        "additionalDirectories": serde_json::from_str::<Value>(additional_directories_json)
            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
        "env": serde_json::from_str::<Value>(env_json)
            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
        "mcpServers": serde_json::from_str::<Value>(mcp_servers_json)
            .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
        "permissionPolicy": permission_policy,
        "skipOsInstructions": skip_os_instructions,
        "additionalInstructions": additional_instructions,
    }))
    .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))
}

fn derive_state_json(
    state: &str,
    started_at_ms: Option<i64>,
    error_json: Option<&str>,
) -> Result<String, VmSqliteError> {
    let value = match state {
        "idle" => serde_json::json!({ "status": "idle" }),
        "running" => serde_json::json!({
            "status": "running",
            "startedAt": timestamp(started_at_ms.ok_or_else(|| VmSqliteError::InvalidResult("running session is missing state_started_at_ms".to_owned()))?)?,
        }),
        "waiting" => serde_json::json!({
            "status": "waiting",
            "waitingSince": timestamp(started_at_ms.ok_or_else(|| VmSqliteError::InvalidResult("waiting session is missing state_started_at_ms".to_owned()))?)?,
            "requests": [],
        }),
        "failed" => serde_json::json!({
            "status": "failed",
            "error": serde_json::from_str::<Value>(error_json.ok_or_else(|| VmSqliteError::InvalidResult("failed session is missing authoritative prompt error".to_owned()))?)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?,
        }),
        other => {
            return Err(VmSqliteError::InvalidResult(format!(
                "invalid stored session state {other:?}"
            )))
        }
    };
    serde_json::to_string(&value).map_err(|error| VmSqliteError::InvalidResult(error.to_string()))
}

fn decode_session(row: &[SqlValue]) -> Result<StoredSession, VmSqliteError> {
    if row.len() != 26 {
        return Err(VmSqliteError::InvalidResult(format!(
            "session row has {} columns, expected 26",
            row.len()
        )));
    }
    let state = required_text(row, 3, "state")?;
    let state_started_at_ms = nullable_integer(row, 5, "state_started_at_ms")?;
    let state_json = derive_state_json(
        &state,
        state_started_at_ms,
        nullable_text(row, 25, "state_error_json")?.as_deref(),
    )?;
    let cwd = required_text(row, 6, "cwd")?;
    let permission_policy = required_text(row, 7, "permission_policy")?;
    let skip_os_instructions = required_boolean(row, 8, "skip_os_instructions")?;
    let additional_instructions = nullable_text(row, 9, "additional_instructions")?;
    let additional_directories_json = required_text(row, 10, "additional_directories_json")?;
    let env_json = required_text(row, 11, "env_json")?;
    let mcp_servers_json = required_text(row, 12, "mcp_servers_json")?;
    let creation_options_json = creation_options_json(
        &cwd,
        &permission_policy,
        skip_os_instructions,
        additional_instructions.as_deref(),
        &additional_directories_json,
        &env_json,
        &mcp_servers_json,
    )?;
    Ok(StoredSession {
        session_id: required_text(row, 0, "session_id")?,
        agent: required_text(row, 1, "agent")?,
        acp_session_id: nullable_text(row, 2, "acp_session_id")?,
        state,
        state_json,
        cwd,
        creation_options_json,
        permission_policy,
        skip_os_instructions,
        additional_instructions,
        additional_directories_json,
        env_json,
        mcp_servers_json,
        capabilities_json: nullable_text(row, 13, "capabilities_json")?,
        agent_info_json: nullable_text(row, 14, "agent_info_json")?,
        config_revision: required_integer(row, 15, "config_revision")?,
        config_options_json: required_text(row, 16, "config_options_json")?,
        title: nullable_text(row, 17, "title")?,
        metadata_json: nullable_text(row, 18, "metadata_json")?,
        latest_sequence: required_integer(row, 19, "latest_sequence")?,
        oldest_retained_sequence: required_integer(row, 20, "oldest_retained_sequence")?,
        created_at_ms: required_integer(row, 23, "created_at_ms")?,
        updated_at_ms: required_integer(row, 24, "updated_at_ms")?,
    })
}

fn decode_session_summary(row: &[SqlValue]) -> Result<StoredSessionSummary, VmSqliteError> {
    if row.len() != 12 {
        return Err(VmSqliteError::InvalidResult(format!(
            "session summary row has {} columns, expected 12",
            row.len()
        )));
    }
    let state = required_text(row, 2, "state")?;
    Ok(StoredSessionSummary {
        session_id: required_text(row, 0, "session_id")?,
        agent: required_text(row, 1, "agent")?,
        state_json: derive_state_json(
            &state,
            nullable_integer(row, 3, "state_started_at_ms")?,
            nullable_text(row, 11, "state_error_json")?.as_deref(),
        )?,
        state,
        cwd: required_text(row, 4, "cwd")?,
        additional_directories_json: required_text(row, 5, "additional_directories_json")?,
        latest_sequence: required_integer(row, 6, "latest_sequence")?,
        created_at_ms: required_integer(row, 7, "created_at_ms")?,
        updated_at_ms: required_integer(row, 8, "updated_at_ms")?,
        title: nullable_text(row, 9, "title")?,
        metadata_json: nullable_text(row, 10, "metadata_json")?,
    })
}

fn decode_events(result: &QueryResult) -> Result<Vec<StoredEvent>, VmSqliteError> {
    result
        .rows
        .iter()
        .map(|row| {
            if row.len() != 7 {
                return Err(VmSqliteError::InvalidResult(
                    "event row must have 7 columns".to_owned(),
                ));
            }
            let kind = required_text(row, 2, "event_kind")?;
            let correlation_id = nullable_text(row, 3, "correlation_id")?;
            let payload: Value = serde_json::from_str(&required_text(row, 4, "payload_json")?)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
            let event = match kind.as_str() {
                "session_update" => serde_json::json!({
                    "type": "session_update",
                    "update": payload,
                }),
                "permission_request" => serde_json::json!({
                    "type": "permission_request",
                    "requestId": correlation_id,
                    "request": payload,
                }),
                "permission_response" => {
                    let mut event = serde_json::json!({
                        "type": "permission_response",
                        "requestId": correlation_id,
                        "response": payload,
                        "status": nullable_text(row, 5, "outcome_status")?,
                    });
                    if let Some(reason) = nullable_text(row, 6, "terminal_reason")? {
                        event["reason"] = Value::String(reason);
                    }
                    event
                }
                other => {
                    return Err(VmSqliteError::InvalidResult(format!(
                        "unknown durable event kind {other:?}"
                    )))
                }
            };
            validate_event(&event)?;
            let event_json = serde_json::to_string(&event)
                .map_err(|error| VmSqliteError::InvalidResult(error.to_string()))?;
            Ok(StoredEvent {
                sequence: required_integer(row, 0, "sequence")?,
                occurred_at_ms: required_integer(row, 1, "occurred_at_ms")?,
                event_json,
            })
        })
        .collect()
}

fn decode_pending_resolution(
    state_result: Option<&QueryResult>,
    event_result: Option<&QueryResult>,
) -> Result<PendingRequestResolution, VmSqliteError> {
    let Some(row) = state_result.and_then(|result| result.rows.first()) else {
        return Ok(PendingRequestResolution::NotFound);
    };
    let state = required_text(row, 0, "state")?;
    let reason =
        nullable_text(row, 1, "terminal_reason")?.unwrap_or_else(|| match state.as_str() {
            "responded" => "already_resolved".to_owned(),
            "pending" => "already_resolved".to_owned(),
            _ => "already_resolved".to_owned(),
        });
    let event = event_result
        .map(decode_events)
        .transpose()?
        .and_then(|events| events.into_iter().next());
    Ok(PendingRequestResolution::Terminal { reason, event })
}

fn decode_prompt(row: &[SqlValue]) -> Result<StoredPrompt, VmSqliteError> {
    if row.len() != 5 {
        return Err(VmSqliteError::InvalidResult(format!(
            "prompt row has {} columns, expected 5",
            row.len()
        )));
    }
    let input_hash = match row.get(1) {
        Some(SqlValue::SqlBlob(value)) => value.clone(),
        value => {
            return Err(VmSqliteError::InvalidResult(format!(
                "input_hash was not a blob: {value:?}"
            )));
        }
    };
    Ok(StoredPrompt {
        prompt_id: required_text(row, 0, "prompt_id")?,
        input_hash,
        state: required_text(row, 2, "state")?,
        result_json: nullable_text(row, 3, "result_json")?,
        error_json: nullable_text(row, 4, "error_json")?,
    })
}

fn required_text(row: &[SqlValue], index: usize, field: &str) -> Result<String, VmSqliteError> {
    match row.get(index) {
        Some(SqlValue::SqlText(value)) => Ok(value.clone()),
        value => Err(VmSqliteError::InvalidResult(format!(
            "{field} was not text: {value:?}"
        ))),
    }
}

fn nullable_text(
    row: &[SqlValue],
    index: usize,
    field: &str,
) -> Result<Option<String>, VmSqliteError> {
    match row.get(index) {
        Some(SqlValue::SqlNull) => Ok(None),
        Some(SqlValue::SqlText(value)) => Ok(Some(value.clone())),
        value => Err(VmSqliteError::InvalidResult(format!(
            "{field} was not nullable text: {value:?}"
        ))),
    }
}

fn required_integer(row: &[SqlValue], index: usize, field: &str) -> Result<i64, VmSqliteError> {
    match row.get(index) {
        Some(SqlValue::SqlInteger(value)) => Ok(*value),
        value => Err(VmSqliteError::InvalidResult(format!(
            "{field} was not an integer: {value:?}"
        ))),
    }
}

fn nullable_integer(
    row: &[SqlValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>, VmSqliteError> {
    match row.get(index) {
        Some(SqlValue::SqlNull) => Ok(None),
        Some(SqlValue::SqlInteger(value)) => Ok(Some(*value)),
        value => Err(VmSqliteError::InvalidResult(format!(
            "{field} was not a nullable integer: {value:?}"
        ))),
    }
}

fn required_boolean(row: &[SqlValue], index: usize, field: &str) -> Result<bool, VmSqliteError> {
    match required_integer(row, index, field)? {
        0 => Ok(false),
        1 => Ok(true),
        value => Err(VmSqliteError::InvalidResult(format!(
            "{field} was not a SQLite boolean: {value}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_native_sidecar::vm_sqlite::resolve_vm_sqlite;
    use agentos_vm_config::VmSqliteDescriptor;

    fn runtime() -> &'static agentos_runtime_tokio::SidecarRuntime {
        agentos_runtime_tokio::SidecarRuntime::process(
            &agentos_runtime_tokio::RuntimeConfig::default(),
        )
        .expect("runtime")
    }

    #[test]
    fn history_and_pending_responses_survive_reopen() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("agentos.sqlite");
            let descriptor = VmSqliteDescriptor::SqliteFile {
                path: path.display().to_string(),
            };
            let database = resolve_vm_sqlite(
                &descriptor,
                context.clone(),
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            store
                .create(
                    "main",
                    "test-agent",
                    "private-1",
                    "/workspace",
                    r#"{"permissionPolicy":"ask"}"#,
                    Some(r#"{"loadSession":true}"#),
                    Some(r#"{"name":"test-agent"}"#),
                    "[]",
                )
                .await
                .expect("create");
            let user = serde_json::json!({
                "sessionUpdate": "user_message_chunk",
                "content": { "type": "text", "text": "hello" },
                "messageId": "user-1",
            });
            let accepted = store
                .accept_prompt(
                    "main",
                    "prompt-1",
                    Some("idem-1"),
                    vec![1; 32],
                    &[user],
                )
                .await
                .expect("accept");
            assert_eq!(accepted[0].sequence, 1);
            store
                .create_pending_request(
                    "main",
                    "prompt-1",
                    "permission-1",
                    "permission",
                    r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-1"},"options":[]}"#,
                )
                .await
                .expect("pending");
            assert!(matches!(store
                .respond_pending_request(
                    "main",
                    "prompt-1",
                    "permission-1",
                    r#"{"outcome":{"outcome":"cancelled"}}"#,
                )
                .await
                .expect("first response"), PendingRequestResolution::Accepted(_)));
            assert!(matches!(store
                .respond_pending_request(
                    "main",
                    "prompt-1",
                    "permission-1",
                    r#"{"outcome":{"outcome":"cancelled"}}"#,
                )
                .await
                .expect("duplicate response"), PendingRequestResolution::Terminal { reason, .. } if reason == "accepted"));
            let agent = serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "world" },
                "messageId": "agent-1",
            });
            store
                .finish_prompt(
                    "main",
                    "prompt-1",
                    &[agent],
                    None,
                    Some(r#"{"stopReason":"end_turn"}"#),
                    None,
                )
                .await
                .expect("finish");
            drop(store);

            let database = resolve_vm_sqlite(
                &descriptor,
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("reopen database");
            let reopened = SessionStore::open(database).await.expect("reopen store");
            let session = reopened.get("main").await.expect("get").expect("session");
            assert_eq!(session.state, "idle");
            assert_eq!(session.latest_sequence, 4);
            let history = reopened
                .read_history(&session, None, None, 100)
                .await
                .expect("history");
            assert_eq!(history.events.len(), 4);
            assert_eq!(history.events[0].sequence, 1);
            assert_eq!(history.events[3].sequence, 4);
            let kinds = history.events.iter().map(|event| {
                serde_json::from_str::<Value>(&event.event_json)
                    .expect("event JSON")
                    .get("type")
                    .and_then(Value::as_str)
                    .expect("event type")
                    .to_owned()
            }).collect::<Vec<_>>();
            assert_eq!(kinds, [
                "session_update",
                "permission_request",
                "permission_response",
                "session_update",
            ]);
        });
    }

    #[test]
    fn invalid_update_does_not_allocate_a_sequence() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let descriptor = VmSqliteDescriptor::SqliteFile {
                path: dir.path().join("state.sqlite").display().to_string(),
            };
            let database = resolve_vm_sqlite(
                &descriptor,
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            store
                .create(
                    "main",
                    "agent",
                    "private",
                    "/workspace",
                    "{}",
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create");
            store
                .append_updates(
                    "main",
                    1,
                    &[serde_json::json!({ "sessionUpdate": "invented_update" })],
                )
                .await
                .expect_err("invalid update");
            assert_eq!(
                store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .latest_sequence,
                0
            );
        });
    }

    #[test]
    fn durable_event_kinds_are_validated_before_storage() {
        let valid_request = serde_json::json!({
            "type": "permission_request",
            "requestId": "r1",
            "request": {
                "sessionId": "main",
                "toolCall": { "toolCallId": "tool-1" },
                "options": [],
            },
        });
        assert_eq!(
            validate_event(&valid_request).expect("valid request"),
            "permission_request"
        );
        assert!(validate_event(&serde_json::json!({
            "type": "permission_request",
            "requestId": "",
            "request": valid_request["request"].clone(),
        }))
        .is_err());
        assert!(validate_event(&serde_json::json!({
            "type": "permission_request",
            "requestId": "r1",
            "request": { "options": [] },
        }))
        .is_err());
        assert!(validate_event(&serde_json::json!({
            "type": "permission_response",
            "requestId": "r1",
            "response": { "outcome": { "outcome": "cancelled" } },
            "status": "accepted",
            "reason": "adapter_exited",
        }))
        .is_err());
        assert!(validate_event(&serde_json::json!({
            "type": "permission_response",
            "requestId": "r1",
            "response": { "outcome": { "outcome": "cancelled" } },
            "status": "not_pending",
            "reason": "invented_reason",
        }))
        .is_err());
    }

    #[test]
    fn history_retention_prunes_oldest_events_by_count_and_bytes() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("retention.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");

            let mut limits = AcpLimits {
                max_session_history_events: 3,
                max_session_history_bytes: 1_000_000,
                ..AcpLimits::default()
            };
            let store = SessionStore::open(database.clone())
                .await
                .expect("store")
                .with_limits(&limits);
            store
                .create(
                    "count",
                    "agent",
                    "private-count",
                    "/workspace",
                    "{}",
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create count session");
            let updates = (1..=5)
                .map(|index| {
                    serde_json::json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": format!("message-{index}") },
                    })
                })
                .collect::<Vec<_>>();
            for update in &updates {
                store
                    .append_updates("count", 1, std::slice::from_ref(update))
                    .await
                    .expect("append count-limited history");
            }
            let count_session = store.get("count").await.expect("get count").expect("count");
            assert_eq!(count_session.latest_sequence, 5);
            assert_eq!(count_session.oldest_retained_sequence, 3);
            let count_history = store
                .read_history(&count_session, None, None, 10)
                .await
                .expect("read count history");
            assert_eq!(
                count_history
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![3, 4, 5]
            );
            assert!(matches!(
                store.append_updates("count", 1, &updates[..4]).await,
                Err(VmSqliteError::HistoryEventBatchTooLarge { used: 4, limit: 3 })
            ));
            let unchanged = store
                .get("count")
                .await
                .expect("get after rejection")
                .expect("count");
            assert_eq!(unchanged.latest_sequence, 5);
            assert_eq!(unchanged.oldest_retained_sequence, 3);

            let byte_updates = ["aaaa", "bbbb", "cccc"].map(|message| {
                serde_json::json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": { "type": "text", "text": message },
                })
            });
            let one_event_bytes =
                serde_json::to_vec(&session_update_event(byte_updates[0].clone()))
                    .expect("serialize update")
                    .len();
            limits.max_session_history_events = 100;
            limits.max_session_history_bytes = one_event_bytes * 2;
            let byte_store = SessionStore::open(database)
                .await
                .expect("byte store")
                .with_limits(&limits);
            byte_store
                .create(
                    "bytes",
                    "agent",
                    "private-bytes",
                    "/workspace",
                    "{}",
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create byte session");
            for update in &byte_updates {
                byte_store
                    .append_updates("bytes", 1, std::slice::from_ref(update))
                    .await
                    .expect("append byte-limited history");
            }
            let byte_session = byte_store
                .get("bytes")
                .await
                .expect("get bytes")
                .expect("bytes");
            assert_eq!(byte_session.latest_sequence, 3);
            assert_eq!(byte_session.oldest_retained_sequence, 2);
            let history = byte_store
                .read_history(&byte_session, None, None, 10)
                .await
                .expect("read retained byte history");
            assert_eq!(
                history
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![2, 3]
            );
            let oversized_update = serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": "x".repeat(one_event_bytes * 2) },
            });
            assert!(matches!(
                byte_store
                    .append_updates("bytes", 1, &[oversized_update])
                    .await,
                Err(VmSqliteError::HistoryByteBatchTooLarge { .. })
            ));
            assert_eq!(
                byte_store
                    .get("bytes")
                    .await
                    .expect("get after byte rejection")
                    .expect("bytes")
                    .latest_sequence,
                3
            );
        });
    }

    #[test]
    fn request_scoped_limits_prune_after_wake_and_refresh_cursor_bounds() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir
                        .path()
                        .join("wake-retention.sqlite")
                        .display()
                        .to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let bootstrap_store = SessionStore::open(database.clone()).await.expect("store");
            bootstrap_store
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
                .expect("create");
            for index in 1..=5 {
                bootstrap_store
                    .append_updates(
                        "main",
                        1,
                        &[serde_json::json!({
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": format!("message-{index}") },
                        })],
                    )
                    .await
                    .expect("append");
            }
            let stale = bootstrap_store
                .get("main")
                .await
                .expect("get")
                .expect("session");
            assert_eq!(stale.oldest_retained_sequence, 1);

            let limits = AcpLimits {
                max_session_history_events: 2,
                ..AcpLimits::default()
            };
            let request_store = SessionStore::from_database(database).with_limits(&limits);
            let refreshed = request_store
                .enforce_history_retention("main")
                .await
                .expect("enforce request limits")
                .expect("session");
            assert_eq!(refreshed.latest_sequence, 5);
            assert_eq!(refreshed.oldest_retained_sequence, 4);
            assert!(1i64.saturating_add(1) < refreshed.oldest_retained_sequence);
            let history = request_store
                .read_history(&refreshed, None, None, 10)
                .await
                .expect("history");
            assert_eq!(
                history
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![4, 5]
            );
        });
    }

    #[test]
    fn interrupted_turn_reconciliation_is_terminal_and_retryable() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("reconcile.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database.clone()).await.expect("store");
            store
                .create(
                    "main",
                    "agent",
                    "private",
                    "/workspace",
                    "{}",
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create");
            store
                .accept_prompt("main", "prompt-1", None, vec![1; 32], &[])
                .await
                .expect("accept prompt");
            store
                .create_pending_request(
                    "main",
                    "prompt-1",
                    "permission-1",
                    "permission",
                    r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-1"},"options":[]}"#,
                )
                .await
                .expect("pending request");

            store
                .reconcile_interrupted_turns()
                .await
                .expect("reconcile");
            drop(store);
            let store = SessionStore::open(database.clone())
                .await
                .expect("reopen reconciled store");
            let session = store.get("main").await.expect("get").expect("session");
            assert_eq!(session.state, "failed");
            let prompt = database
                .query(SqlStatement::new(
                    "SELECT state FROM agentos_core_prompts WHERE session_id = ? AND prompt_id = ?",
                    vec![text("main"), text("prompt-1")],
                ))
                .await
                .expect("prompt query");
            assert_eq!(
                prompt.rows,
                vec![vec![SqlValue::SqlText(String::from("failed"))]]
            );
            let pending = database
                .query(SqlStatement::new(
                    "SELECT state FROM agentos_core_permission_records WHERE session_id = ? AND request_id = ?",
                    vec![text("main"), text("permission-1")],
                ))
                .await
                .expect("pending query");
            assert_eq!(
                pending.rows,
                vec![vec![SqlValue::SqlText(String::from("terminal"))]]
            );
            assert!(matches!(
                store
                    .pending_request_resolution("main", "permission-1")
                    .await
                    .expect("terminal lookup"),
                PendingRequestResolution::Terminal { reason, .. } if reason == "vm_shutdown"
            ));

            store
                .accept_prompt("main", "prompt-2", None, vec![2; 32], &[])
                .await
                .expect("explicit retry is accepted");
            assert_eq!(
                store
                    .get("main")
                    .await
                    .expect("get retry")
                    .expect("session")
                    .state,
                "running"
            );
        });
    }

    async fn create_waiting_permission(
        store: &SessionStore,
        session_id: &str,
        prompt_id: &str,
        request_id: &str,
    ) {
        store
            .create(
                session_id,
                "agent",
                &format!("acp-{session_id}"),
                "/workspace",
                r#"{"permissionPolicy":"ask"}"#,
                None,
                None,
                "[]",
            )
            .await
            .expect("create session");
        store
            .accept_prompt(session_id, prompt_id, None, vec![1; 32], &[])
            .await
            .expect("accept prompt");
        store
            .create_pending_request(
                session_id,
                prompt_id,
                request_id,
                "permission",
                r#"{"sessionId":"public","toolCall":{"toolCallId":"tool-1"},"options":[]}"#,
            )
            .await
            .expect("create pending permission");
    }

    #[test]
    fn permission_response_and_cancellation_are_first_writer_wins() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("permission-race.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            create_waiting_permission(&store, "race", "prompt", "request").await;

            let response_store = store.clone();
            let cancellation_store = store.clone();
            let (response, cancellation) = tokio::join!(
                response_store.respond_pending_request(
                    "race",
                    "prompt",
                    "request",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                ),
                cancellation_store.terminate_pending_request(
                    "race",
                    "prompt",
                    "request",
                    "prompt_cancelled",
                )
            );
            let response = response.expect("response race result");
            let cancellation = cancellation.expect("cancellation race result");
            match response {
                PendingRequestResolution::Accepted(_) => assert!(matches!(
                    cancellation,
                    PendingRequestResolution::Terminal { reason, .. } if reason == "accepted"
                )),
                PendingRequestResolution::Terminal { reason, .. } => {
                    assert_eq!(reason, "prompt_cancelled");
                    assert!(matches!(
                        cancellation,
                        PendingRequestResolution::Terminal { reason, .. } if reason == "prompt_cancelled"
                    ));
                }
                PendingRequestResolution::NotFound => panic!("race lost the durable request"),
            }

            let session = store.get("race").await.expect("get").expect("session");
            let history = store
                .read_history(&session, None, None, 100)
                .await
                .expect("history");
            assert_eq!(
                history
                    .events
                    .iter()
                    .filter(|event| {
                        serde_json::from_str::<Value>(&event.event_json)
                            .ok()
                            .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_owned))
                            .as_deref()
                            == Some("permission_response")
                    })
                    .count(),
                1,
                "the winning terminal transition must allocate exactly one response event"
            );
        });
    }

    #[test]
    fn permission_response_races_adapter_exit_vm_shutdown_and_session_deletion() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir
                        .path()
                        .join("permission-lifecycle-races.sqlite")
                        .display()
                        .to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");

            create_waiting_permission(&store, "exit-race", "prompt", "request").await;
            let (response, exit) = tokio::join!(
                store.respond_pending_request(
                    "exit-race",
                    "prompt",
                    "request",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                ),
                store
                    .terminate_pending_request("exit-race", "prompt", "request", "adapter_exited",)
            );
            let response = response.expect("response/exit race");
            let exit = exit.expect("exit/response race");
            match response {
                PendingRequestResolution::Accepted(_) => assert!(matches!(
                    exit,
                    PendingRequestResolution::Terminal { reason, .. } if reason == "accepted"
                )),
                PendingRequestResolution::Terminal { reason, .. } => {
                    assert_eq!(reason, "adapter_exited")
                }
                PendingRequestResolution::NotFound => panic!("exit race lost request"),
            }

            create_waiting_permission(&store, "shutdown-race", "prompt", "request").await;
            let (response, shutdown) = tokio::join!(
                store.respond_pending_request(
                    "shutdown-race",
                    "prompt",
                    "request",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                ),
                store.reconcile_interrupted_turns()
            );
            response.expect("response/shutdown race");
            shutdown.expect("shutdown reconciliation race");
            assert!(matches!(
                store
                    .pending_request_resolution("shutdown-race", "request")
                    .await
                    .expect("shutdown resolution"),
                PendingRequestResolution::Terminal { reason, .. }
                    if reason == "accepted" || reason == "vm_shutdown"
            ));

            create_waiting_permission(&store, "delete-race", "prompt", "request").await;
            let (response, deletion) = tokio::join!(
                store.respond_pending_request(
                    "delete-race",
                    "prompt",
                    "request",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                ),
                store.delete("delete-race")
            );
            response.expect("response/delete race");
            deletion.expect("delete/response race");
            assert!(matches!(
                store
                    .pending_request_resolution("delete-race", "request")
                    .await
                    .expect("deleted resolution"),
                PendingRequestResolution::Terminal { reason, .. }
                    if reason == "accepted" || reason == "session_deleted"
            ));
        });
    }

    #[test]
    fn permission_terminal_reasons_survive_lifecycle_cleanup() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("permission-lifecycle.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");

            for (session_id, reason) in [
                ("cancelled", "prompt_cancelled"),
                ("exited", "adapter_exited"),
            ] {
                create_waiting_permission(&store, session_id, "prompt", "request").await;
                store
                    .terminate_pending_request(session_id, "prompt", "request", reason)
                    .await
                    .expect("terminate permission");
                assert!(matches!(
                    store.pending_request_resolution(session_id, "request").await.expect("resolution"),
                    PendingRequestResolution::Terminal { reason: actual, .. } if actual == reason
                ));
            }

            create_waiting_permission(&store, "deleted", "prompt", "request").await;
            store.delete("deleted").await.expect("delete session");
            assert!(matches!(
                store.pending_request_resolution("deleted", "request").await.expect("deleted resolution"),
                PendingRequestResolution::Terminal { reason, .. } if reason == "session_deleted"
            ));
            assert_eq!(
                store
                    .pending_request_resolution("missing", "unknown")
                    .await
                    .expect("missing resolution"),
                PendingRequestResolution::NotFound
            );
        });
    }

    #[test]
    fn history_cursor_and_sequence_deduplicate_reconnect_overlap() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir
                        .path()
                        .join("permission-reconnect.sqlite")
                        .display()
                        .to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            create_waiting_permission(&store, "main", "prompt", "request").await;
            store
                .respond_pending_request(
                    "main",
                    "prompt",
                    "request",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                )
                .await
                .expect("respond");

            let session = store.get("main").await.expect("get").expect("session");
            let recovered = store
                .read_history(&session, None, Some(1), 100)
                .await
                .expect("history after cursor");
            assert_eq!(
                recovered
                    .events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![2]
            );

            // A reconnecting subscriber can receive sequence 2 live while its
            // history fetch is in flight. Sequence is the stable dedupe key.
            let mut combined = recovered.events.clone();
            combined.push(recovered.events[0].clone());
            combined.sort_by_key(|event| event.sequence);
            combined.dedup_by_key(|event| event.sequence);
            assert_eq!(
                combined
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>(),
                vec![2]
            );
        });
    }

    #[test]
    fn permission_tombstone_pruning_retains_newest_entries() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("tombstones.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            SessionStore::open(database.clone()).await.expect("store");
            let mut statements = (1..=4)
                .map(|index| SqlStatement::new(
                    "INSERT INTO agentos_core_permission_outcomes (session_id, request_id, terminal_reason, terminal_at_ms) VALUES (?, ?, 'session_deleted', ?)",
                    vec![text("main"), text(&format!("request-{index}")), SqlValue::SqlInteger(index)],
                ))
                .collect::<Vec<_>>();
            statements.push(permission_outcome_prune_statement(2));
            database.transaction(statements).await.expect("prune");
            let retained = database.query(SqlStatement::new(
                "SELECT request_id FROM agentos_core_permission_outcomes ORDER BY terminal_at_ms",
                vec![],
            )).await.expect("retained");
            assert_eq!(retained.rows, vec![
                vec![text("request-3")],
                vec![text("request-4")],
            ]);
        });
    }

    #[test]
    fn core_schema_is_strict_namespaced_and_has_no_duplicate_state() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("schema.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            SessionStore::open(database.clone()).await.expect("store");

            let tables = database
                .query(SqlStatement::plain(
                    "SELECT name, strict FROM pragma_table_list WHERE name LIKE 'agentos_%' ORDER BY name",
                ))
                .await
                .expect("table list");
            assert_eq!(
                tables.rows,
                [
                    "agentos_core_events",
                    "agentos_core_permission_outcomes",
                    "agentos_core_permission_records",
                    "agentos_core_prompts",
                    "agentos_core_schema_version",
                    "agentos_core_sessions",
                ]
                .into_iter()
                .map(|name| vec![text(name), SqlValue::SqlInteger(1)])
                .collect::<Vec<_>>()
            );

            for table in [
                "agentos_core_events",
                "agentos_core_permission_outcomes",
                "agentos_core_permission_records",
                "agentos_core_prompts",
                "agentos_core_sessions",
            ] {
                let foreign_keys = database
                    .query(SqlStatement::plain(format!("PRAGMA foreign_key_list({table})")))
                    .await
                    .expect("foreign key list");
                assert!(foreign_keys.rows.is_empty(), "{table} must not use foreign keys");
            }

            let columns = database
                .query(SqlStatement::plain(
                    "SELECT m.name, p.name FROM sqlite_master m JOIN pragma_table_info(m.name) p WHERE m.name IN ('agentos_core_sessions', 'agentos_core_prompts', 'agentos_core_events') ORDER BY m.name, p.cid",
                ))
                .await
                .expect("columns");
            let names = columns
                .rows
                .iter()
                .map(|row| {
                    format!(
                        "{}.{}",
                        required_text(row, 0, "table").expect("table"),
                        required_text(row, 1, "column").expect("column")
                    )
                })
                .collect::<Vec<_>>();
            for removed in [
                "agentos_core_sessions.state_json",
                "agentos_core_sessions.creation_options_json",
                "agentos_core_prompts.input_json",
                "agentos_core_events.update_type",
                "agentos_core_events.update_json",
            ] {
                assert!(!names.iter().any(|name| name == removed), "legacy column {removed}");
            }
        });
    }

    #[test]
    fn derived_session_state_tracks_prompts_and_permissions() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("state.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            store
                .create(
                    "main",
                    "agent",
                    "native",
                    "/workspace",
                    r#"{"formatVersion":1,"permissionPolicy":"ask"}"#,
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create");
            assert_eq!(
                serde_json::from_str::<Value>(
                    &store
                        .get("main")
                        .await
                        .expect("get")
                        .expect("session")
                        .state_json
                )
                .expect("state"),
                serde_json::json!({"status":"idle"})
            );

            store
                .accept_prompt("main", "p1", Some("idem-1"), vec![1; 32], &[])
                .await
                .expect("accept");
            assert_eq!(
                store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .state,
                "running"
            );
            store
                .create_pending_request(
                    "main",
                    "p1",
                    "r1",
                    "permission",
                    r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-1"},"options":[]}"#,
                )
                .await
                .expect("pending");
            let waiting: Value = serde_json::from_str(
                &store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .state_json,
            )
            .expect("waiting state");
            assert_eq!(waiting["status"], "waiting");
            assert_eq!(waiting["requests"][0]["requestId"], "r1");
            store
                .respond_pending_request(
                    "main",
                    "p1",
                    "r1",
                    r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#,
                )
                .await
                .expect("respond");
            assert_eq!(
                store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .state,
                "running"
            );
            store
                .finish_prompt(
                    "main",
                    "p1",
                    &[],
                    None,
                    Some(r#"{"stopReason":"end_turn"}"#),
                    None,
                )
                .await
                .expect("finish");
            assert_eq!(
                store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .state,
                "idle"
            );

            store
                .accept_prompt("main", "p2", None, vec![2; 32], &[])
                .await
                .expect("accept failed turn");
            let error = r#"{"code":"adapter_failed","message":"boom"}"#;
            store
                .finish_prompt("main", "p2", &[], None, None, Some(error))
                .await
                .expect("fail");
            let failed: Value = serde_json::from_str(
                &store
                    .get("main")
                    .await
                    .expect("get")
                    .expect("session")
                    .state_json,
            )
            .expect("failed state");
            assert_eq!(failed["status"], "failed");
            assert_eq!(
                failed["error"],
                serde_json::from_str::<Value>(error).expect("error")
            );
        });
    }

    #[test]
    fn prompt_hash_and_terminal_result_are_sufficient_after_reopen() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let descriptor = VmSqliteDescriptor::SqliteFile {
                path: dir.path().join("idempotency.sqlite").display().to_string(),
            };
            let database = resolve_vm_sqlite(
                &descriptor,
                context.clone(),
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
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
                .expect("create");
            let input_hash = vec![7; 32];
            let user = serde_json::json!({
                "sessionUpdate": "user_message_chunk",
                "content": {"type":"text", "text":"durable input"}
            });
            store
                .accept_prompt("main", "p1", Some("same"), input_hash.clone(), &[user])
                .await
                .expect("accept");
            let result = r#"{"stopReason":"end_turn"}"#;
            store
                .finish_prompt("main", "p1", &[], None, Some(result), None)
                .await
                .expect("finish");
            drop(store);

            let database = resolve_vm_sqlite(
                &descriptor,
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("reopen database");
            let reopened = SessionStore::open(database).await.expect("reopen store");
            let prompt = reopened
                .prompt_by_idempotency_key("main", "same")
                .await
                .expect("lookup")
                .expect("prompt");
            assert_eq!(prompt.input_hash, input_hash);
            assert_eq!(prompt.state, "completed");
            assert_eq!(prompt.result_json.as_deref(), Some(result));
        });
    }

    #[test]
    fn session_state_tracks_all_pending_permission_rows() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir
                        .path()
                        .join("multi-pending.sqlite")
                        .display()
                        .to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database).await.expect("store");
            store
                .create(
                    "main",
                    "agent",
                    "native",
                    "/workspace",
                    r#"{"permissionPolicy":"ask"}"#,
                    None,
                    None,
                    "[]",
                )
                .await
                .expect("create");
            store
                .accept_prompt("main", "p1", None, vec![1; 32], &[])
                .await
                .expect("accept");
            for (request_id, tool_call_id) in [("r1", "tool-1"), ("r2", "tool-2")] {
                let request = serde_json::json!({
                    "sessionId": "main",
                    "toolCall": { "toolCallId": tool_call_id },
                    "options": [],
                });
                store
                    .create_pending_request(
                        "main",
                        "p1",
                        request_id,
                        "permission",
                        &serde_json::to_string(&request).expect("request JSON"),
                    )
                    .await
                    .expect("create pending request");
            }

            let waiting = store.get("main").await.expect("get").expect("session");
            let waiting_state: Value =
                serde_json::from_str(&waiting.state_json).expect("waiting state");
            assert_eq!(waiting.state, "waiting");
            assert_eq!(waiting_state["requests"].as_array().map(Vec::len), Some(2));

            store
                .respond_pending_request(
                    "main",
                    "p1",
                    "r1",
                    r#"{"outcome":{"outcome":"cancelled"}}"#,
                )
                .await
                .expect("respond first request");
            let still_waiting = store.get("main").await.expect("get").expect("session");
            let still_waiting_state: Value =
                serde_json::from_str(&still_waiting.state_json).expect("waiting state");
            assert_eq!(still_waiting.state, "waiting");
            assert_eq!(
                still_waiting_state["requests"].as_array().map(Vec::len),
                Some(1)
            );
            assert_eq!(still_waiting_state["requests"][0]["requestId"], "r2");

            store
                .terminate_pending_request("main", "p1", "r2", "adapter_exited")
                .await
                .expect("terminate final request");
            let running = store.get("main").await.expect("get").expect("session");
            assert_eq!(running.state, "running");
            assert_eq!(
                serde_json::from_str::<Value>(&running.state_json).expect("running state")
                    ["status"],
                "running"
            );
        });
    }

    #[test]
    fn durable_collection_limits_prune_terminal_outcomes_and_reject_active_growth() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("bounds.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let limits = AcpLimits {
                max_sessions_per_vm: 1,
                max_prompts_per_session: 1,
                max_prompts_per_vm: 1,
                max_pending_permissions_per_session: 1,
                max_pending_permissions_per_vm: 1,
                max_permission_outcomes_per_session: 1,
                max_permission_outcomes_per_vm: 1,
                ..AcpLimits::default()
            };
            let store = SessionStore::open(database).await.expect("store").with_limits(&limits);
            store.create("main", "agent", "native", "/workspace", r#"{"permissionPolicy":"ask"}"#, None, None, "[]").await.expect("create");
            assert!(matches!(
                store.create("other", "agent", "native-2", "/workspace", "{}", None, None, "[]").await,
                Err(VmSqliteError::DurableCollectionLimit { setting: "limits.acp.maxSessionsPerVm", .. })
            ));
            store.accept_prompt("main", "p1", None, vec![1; 32], &[]).await.expect("accept");
            assert!(matches!(
                store.accept_prompt("main", "p2", None, vec![2; 32], &[]).await,
                Err(VmSqliteError::DurableCollectionLimit { setting: "limits.acp.maxPromptsPerSession", .. })
            ));
            store.create_pending_request("main", "p1", "r1", "permission", r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-1"},"options":[]}"#).await.expect("pending");
            assert!(matches!(
                store.create_pending_request("main", "p1", "r2", "permission", r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-2"},"options":[]}"#).await,
                Err(VmSqliteError::DurableCollectionLimit { setting: "limits.acp.maxPendingPermissionsPerSession", .. })
            ));
            store.respond_pending_request("main", "p1", "r1", r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#).await.expect("respond r1");
            store.create_pending_request("main", "p1", "r2", "permission", r#"{"sessionId":"main","toolCall":{"toolCallId":"tool-2"},"options":[]}"#).await.expect("pending r2");
            store.respond_pending_request("main", "p1", "r2", r#"{"outcome":{"outcome":"selected","optionId":"allow"}}"#).await.expect("respond r2");
            assert_eq!(store.pending_request_resolution("main", "r1").await.expect("old outcome"), PendingRequestResolution::NotFound);
            assert!(matches!(store.pending_request_resolution("main", "r2").await.expect("new outcome"), PendingRequestResolution::Terminal { reason, .. } if reason == "accepted"));
        });
    }

    #[test]
    fn concurrent_admission_cannot_cross_vm_prompt_or_permission_limits() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("concurrent-bounds.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let mut limits = AcpLimits {
                max_prompts_per_session: 1,
                max_prompts_per_vm: 1,
                ..AcpLimits::default()
            };
            let prompt_store = SessionStore::open(database.clone())
                .await
                .expect("store")
                .with_limits(&limits);
            for session_id in ["one", "two"] {
                prompt_store
                    .create(
                        session_id,
                        "agent",
                        session_id,
                        "/workspace",
                        r#"{"permissionPolicy":"ask"}"#,
                        None,
                        None,
                        "[]",
                    )
                    .await
                    .expect("create session");
            }
            let one = prompt_store.clone();
            let two = prompt_store.clone();
            let (one_result, two_result) = tokio::join!(
                one.accept_prompt("one", "p1", None, vec![1; 32], &[]),
                two.accept_prompt("two", "p2", None, vec![2; 32], &[]),
            );
            assert_eq!(usize::from(one_result.is_ok()) + usize::from(two_result.is_ok()), 1);
            let rejected = if one_result.is_err() {
                one_result.expect_err("one rejected")
            } else {
                two_result.expect_err("two rejected")
            };
            assert!(matches!(
                rejected,
                VmSqliteError::DurableCollectionLimit {
                    setting: "limits.acp.maxPromptsPerVm",
                    ..
                }
            ));

            database
                .transaction(vec![
                    SqlStatement::plain("DELETE FROM agentos_core_prompts"),
                    SqlStatement::plain(
                        "UPDATE agentos_core_sessions SET state = 'idle', state_prompt_id = NULL, state_started_at_ms = NULL",
                    ),
                ])
                .await
                .expect("reset prompt fixture");

            limits.max_prompts_per_session = 10;
            limits.max_prompts_per_vm = 10;
            limits.max_pending_permissions_per_session = 1;
            limits.max_pending_permissions_per_vm = 1;
            let permission_store =
                SessionStore::from_database(database).with_limits(&limits);
            permission_store
                .accept_prompt("one", "permission-p1", None, vec![3; 32], &[])
                .await
                .expect("accept one");
            permission_store
                .accept_prompt("two", "permission-p2", None, vec![4; 32], &[])
                .await
                .expect("accept two");
            let request_one = r#"{"sessionId":"one","toolCall":{"toolCallId":"tool-1"},"options":[]}"#;
            let request_two = r#"{"sessionId":"two","toolCall":{"toolCallId":"tool-2"},"options":[]}"#;
            let one = permission_store.clone();
            let two = permission_store.clone();
            let (one_result, two_result) = tokio::join!(
                one.create_pending_request(
                    "one",
                    "permission-p1",
                    "r1",
                    "permission",
                    request_one,
                ),
                two.create_pending_request(
                    "two",
                    "permission-p2",
                    "r2",
                    "permission",
                    request_two,
                ),
            );
            assert_eq!(usize::from(one_result.is_ok()) + usize::from(two_result.is_ok()), 1);
            let rejected = if one_result.is_err() {
                one_result.expect_err("one rejected")
            } else {
                two_result.expect_err("two rejected")
            };
            assert!(matches!(
                rejected,
                VmSqliteError::DurableCollectionLimit {
                    setting: "limits.acp.maxPendingPermissionsPerVm",
                    ..
                }
            ));
        });
    }

    #[test]
    fn corrupt_oldest_retained_sequence_is_reconciled_on_reopen() {
        let runtime = runtime();
        let context = runtime.context();
        runtime.block_on(async move {
            let dir = tempfile::tempdir().expect("tempdir");
            let database = resolve_vm_sqlite(
                &VmSqliteDescriptor::SqliteFile {
                    path: dir.path().join("counter-reconcile.sqlite").display().to_string(),
                },
                context,
                agentos_native_sidecar::limits::DEFAULT_SQLITE_MAX_RESULT_BYTES,
            )
            .await
            .expect("database");
            let store = SessionStore::open(database.clone()).await.expect("store");
            store.create("main", "agent", "native", "/workspace", "{}", None, None, "[]").await.expect("create");
            store.append_updates("main", 1, &[serde_json::json!({
                "sessionUpdate":"agent_message_chunk",
                "content":{"type":"text","text":"one"}
            })]).await.expect("append");
            database.query(SqlStatement::plain(
                "UPDATE agentos_core_sessions SET oldest_retained_sequence = 2 WHERE session_id = 'main'",
            )).await.expect("corrupt retention metadata");
            let reopened = SessionStore::open(database).await.expect("reconcile");
            let session = reopened.get("main").await.expect("get").expect("session");
            assert_eq!(session.oldest_retained_sequence, 1);
            let history = reopened.read_history(&session, None, None, 10).await.expect("history");
            assert_eq!(history.events.len(), 1);
        });
    }
}

#[cfg(test)]
#[path = "session_store/performance_tests.rs"]
mod performance_tests;
