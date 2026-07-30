use crate::bridge::MountPluginContext;
use crate::vm_sqlite::{
    migrate_schema, QueryResult, SharedVmSqliteDatabase, SqlStatement, SqlValue, VmSqliteMigration,
};
use agentos_vfs_core::engine::block::BlockStore;
use agentos_vfs_core::engine::engines::{ChunkedFs, ChunkedFsOptions};
use agentos_vfs_core::engine::error::{VfsError, VfsResult};
use agentos_vfs_core::engine::mem::metadata_store::{InMemoryMetadataStore, MetadataDump};
use agentos_vfs_core::engine::metadata::MetadataStore;
use agentos_vfs_core::engine::types::{
    BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs, DentryStat, InodeMeta, InodePatch,
    SnapshotId,
};
use agentos_vfs_core::engine::CachedMetadataStore;
use agentos_vfs_storage::MountedEngineFileSystem;
use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::MountedFileSystem;
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::{Mutex, OnceCell};

const DEFAULT_METADATA_CACHE_ENTRIES: usize = 4096;
const MAX_METADATA_CACHE_ENTRIES: usize = 1_000_000;
const METADATA_CHUNK_SIZE: usize = 256 * 1024;
const DEFAULT_MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 1024 * 1024 * 1024;
const METADATA_CLEANUP_BATCH_SIZE: i64 = 64;
const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;
const VFS_MIGRATION_1: &[&str] = &[
    "CREATE TABLE agentos_fs_metadata_heads (
    namespace TEXT PRIMARY KEY CHECK (length(namespace) > 0),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    chunk_count INTEGER NOT NULL CHECK (chunk_count >= 0),
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0)
) STRICT",
    "CREATE TABLE agentos_fs_metadata_chunks (
    namespace TEXT NOT NULL CHECK (length(namespace) > 0),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    chunk_index INTEGER NOT NULL CHECK (chunk_index >= 0),
    content BLOB NOT NULL,
    PRIMARY KEY (namespace, generation, chunk_index)
) STRICT",
    "CREATE TABLE agentos_fs_blocks (
    namespace TEXT NOT NULL CHECK (length(namespace) > 0),
    block_key TEXT NOT NULL CHECK (length(block_key) > 0),
    content BLOB NOT NULL,
    PRIMARY KEY (namespace, block_key)
) STRICT",
];

const VFS_MIGRATIONS: &[VmSqliteMigration] = &[VmSqliteMigration {
    version: 1,
    statements: VFS_MIGRATION_1,
}];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ChunkedActorSqliteMountConfig {
    #[serde(default = "default_namespace")]
    namespace: String,
    chunk_size: Option<u32>,
    inline_threshold: Option<usize>,
    uid: Option<u32>,
    gid: Option<u32>,
    file_mode: Option<u32>,
    dir_mode: Option<u32>,
    metadata_cache_entries: Option<usize>,
    max_metadata_bytes: Option<usize>,
}

fn default_namespace() -> String {
    "agentos-root".to_owned()
}

#[derive(Debug)]
pub(crate) struct ChunkedActorSqliteMountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for ChunkedActorSqliteMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "chunked_actor_sqlite"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ChunkedActorSqliteMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        validate_config(&config)?;

        let chunk_size = config
            .chunk_size
            .unwrap_or(agentos_vfs_core::engine::DEFAULT_CHUNK_SIZE);
        let inline_threshold = config
            .inline_threshold
            .unwrap_or(agentos_vfs_core::engine::DEFAULT_INLINE_THRESHOLD);
        let database = request.context.database.clone().ok_or_else(|| {
            PluginError::invalid_input(
                "chunked_actor_sqlite requires createVm database configuration",
            )
        })?;
        let metadata = CachedMetadataStore::new(
            ActorSqliteMetadataStore::new(
                database.clone(),
                config.namespace.clone(),
                config
                    .max_metadata_bytes
                    .unwrap_or(DEFAULT_MAX_METADATA_BYTES),
            ),
            config
                .metadata_cache_entries
                .unwrap_or(DEFAULT_METADATA_CACHE_ENTRIES),
        );
        let blocks = ActorSqliteBlockStore::new(database, config.namespace);
        let fs = ChunkedFs::with_options(
            metadata,
            blocks,
            ChunkedFsOptions {
                inline_threshold,
                chunk_size,
                uid: config.uid.unwrap_or(0),
                gid: config.gid.unwrap_or(0),
                file_mode: config.file_mode.unwrap_or(0o644),
                dir_mode: config.dir_mode.unwrap_or(0o755),
            },
        );
        Ok(Box::new(MountedEngineFileSystem::with_runtime_context(
            fs,
            request.context.runtime_context.clone(),
        )))
    }
}

fn validate_config(config: &ChunkedActorSqliteMountConfig) -> Result<(), PluginError> {
    if config.namespace.is_empty() || config.namespace.len() > 256 {
        return Err(PluginError::invalid_input(
            "chunked_actor_sqlite.namespace must contain 1..=256 bytes",
        ));
    }
    let chunk_size = config
        .chunk_size
        .unwrap_or(agentos_vfs_core::engine::DEFAULT_CHUNK_SIZE);
    if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
        return Err(PluginError::invalid_input(format!(
            "chunked_actor_sqlite.chunkSize must be between 1 and {MAX_CHUNK_SIZE} bytes"
        )));
    }
    let inline_threshold = config
        .inline_threshold
        .unwrap_or(agentos_vfs_core::engine::DEFAULT_INLINE_THRESHOLD);
    if inline_threshold > chunk_size as usize {
        return Err(PluginError::invalid_input(
            "chunked_actor_sqlite.inlineThreshold must not exceed chunkSize",
        ));
    }
    let cache_entries = config
        .metadata_cache_entries
        .unwrap_or(DEFAULT_METADATA_CACHE_ENTRIES);
    if cache_entries > MAX_METADATA_CACHE_ENTRIES {
        return Err(PluginError::invalid_input(format!(
            "chunked_actor_sqlite.metadataCacheEntries exceeds limit {MAX_METADATA_CACHE_ENTRIES}"
        )));
    }
    let max_metadata_bytes = config
        .max_metadata_bytes
        .unwrap_or(DEFAULT_MAX_METADATA_BYTES);
    if max_metadata_bytes == 0 || max_metadata_bytes > MAX_METADATA_BYTES {
        return Err(PluginError::invalid_input(format!(
            "chunked_actor_sqlite.maxMetadataBytes must be between 1 and {MAX_METADATA_BYTES} bytes"
        )));
    }
    Ok(())
}

struct ActorSqliteMetadataStore {
    database: SharedVmSqliteDatabase,
    namespace: String,
    max_metadata_bytes: usize,
    inner: OnceCell<InMemoryMetadataStore>,
    mutation: Mutex<()>,
}

impl ActorSqliteMetadataStore {
    fn new(database: SharedVmSqliteDatabase, namespace: String, max_metadata_bytes: usize) -> Self {
        Self {
            database,
            namespace,
            max_metadata_bytes,
            inner: OnceCell::new(),
            mutation: Mutex::new(()),
        }
    }

    async fn inner(&self) -> VfsResult<&InMemoryMetadataStore> {
        self.inner
            .get_or_try_init(|| async {
                match load_metadata(&self.database, &self.namespace, self.max_metadata_bytes)
                    .await?
                {
                    Some(bytes) => {
                        let dump: MetadataDump =
                            serde_bare::from_slice(&bytes).map_err(|error| {
                                VfsError::eio(format!(
                                    "decode actor SQLite VFS metadata for namespace {}: {error}",
                                    self.namespace
                                ))
                            })?;
                        Ok(InMemoryMetadataStore::from_dump(dump))
                    }
                    None => {
                        let inner = InMemoryMetadataStore::default();
                        persist_metadata(
                            &self.database,
                            &self.namespace,
                            &inner,
                            self.max_metadata_bytes,
                        )
                        .await?;
                        Ok(inner)
                    }
                }
            })
            .await
    }

    async fn persist(&self, inner: &InMemoryMetadataStore) -> VfsResult<()> {
        persist_metadata(
            &self.database,
            &self.namespace,
            inner,
            self.max_metadata_bytes,
        )
        .await
    }
}

pub(crate) async fn bootstrap_schema(
    database: &dyn crate::vm_sqlite::VmSqliteDatabase,
) -> VfsResult<()> {
    migrate_schema(
        database,
        "filesystem",
        "agentos_fs_schema_version",
        VFS_MIGRATIONS,
    )
    .await
    .map_err(actor_sql_error)
}

async fn persist_metadata(
    database: &SharedVmSqliteDatabase,
    namespace: &str,
    inner: &InMemoryMetadataStore,
    max_metadata_bytes: usize,
) -> VfsResult<()> {
    let dump = serde_bare::to_vec(&inner.dump())
        .map_err(|error| VfsError::eio(format!("encode actor SQLite VFS metadata: {error}")))?;
    if dump.len() > max_metadata_bytes {
        return Err(VfsError::new(
            "EFBIG",
            format!(
                "actor SQLite VFS metadata is {} bytes, exceeding maxMetadataBytes={max_metadata_bytes}; raise chunked_actor_sqlite.maxMetadataBytes",
                dump.len()
            ),
        ));
    }
    if dump.len() >= max_metadata_bytes.saturating_mul(4) / 5 {
        eprintln!(
            "agentos chunked_actor_sqlite metadata is nearing maxMetadataBytes: actual={} limit={max_metadata_bytes}",
            dump.len()
        );
    }

    let generation_result = database
        .query(SqlStatement::new(
            "SELECT COALESCE(MAX(generation), 0) FROM agentos_fs_metadata_chunks WHERE namespace = ?",
            vec![SqlValue::SqlText(namespace.to_owned())],
        ))
        .await
        .map_err(actor_sql_error)?;
    let generation = first_integer(generation_result, "metadata generation")?
        .checked_add(1)
        .ok_or_else(|| VfsError::eio("actor SQLite VFS metadata generation overflow"))?;

    let chunks = dump.chunks(METADATA_CHUNK_SIZE).collect::<Vec<_>>();
    for (chunk_index, content) in chunks.iter().enumerate() {
        database
            .query(SqlStatement::new(
                "INSERT INTO agentos_fs_metadata_chunks (namespace, generation, chunk_index, content) VALUES (?, ?, ?, ?)",
                vec![
                    SqlValue::SqlText(namespace.to_owned()),
                    SqlValue::SqlInteger(generation),
                    SqlValue::SqlInteger(i64::try_from(chunk_index).map_err(|_| {
                        VfsError::eio("actor SQLite VFS metadata chunk index overflow")
                    })?),
                    SqlValue::SqlBlob(content.to_vec()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
    }

    database
        .query(SqlStatement::new(
            "INSERT INTO agentos_fs_metadata_heads (namespace, generation, chunk_count, byte_length) VALUES (?, ?, ?, ?) \
             ON CONFLICT(namespace) DO UPDATE SET generation = excluded.generation, chunk_count = excluded.chunk_count, byte_length = excluded.byte_length",
            vec![
                SqlValue::SqlText(namespace.to_owned()),
                SqlValue::SqlInteger(generation),
                SqlValue::SqlInteger(i64::try_from(chunks.len()).map_err(|_| {
                    VfsError::eio("actor SQLite VFS metadata chunk count overflow")
                })?),
                SqlValue::SqlInteger(i64::try_from(dump.len()).map_err(|_| {
                    VfsError::eio("actor SQLite VFS metadata byte length overflow")
                })?),
            ],
        ))
        .await
        .map_err(actor_sql_error)?;

    if let Err(error) = cleanup_old_metadata(database, namespace, generation).await {
        eprintln!(
            "agentos chunked_actor_sqlite failed to clean superseded metadata generations: {error}"
        );
    }
    Ok(())
}

async fn load_metadata(
    database: &SharedVmSqliteDatabase,
    namespace: &str,
    max_metadata_bytes: usize,
) -> VfsResult<Option<Vec<u8>>> {
    let result = database
        .query(SqlStatement::new(
            "SELECT generation, chunk_count, byte_length FROM agentos_fs_metadata_heads WHERE namespace = ?",
            vec![SqlValue::SqlText(namespace.to_owned())],
        ))
        .await
        .map_err(actor_sql_error)?;
    let Some(row) = result.rows.into_iter().next() else {
        return Ok(None);
    };
    if row.len() != 3 {
        return Err(VfsError::eio(
            "actor SQLite returned malformed VFS metadata head",
        ));
    }
    let generation = sql_nonnegative_integer(&row[0], "metadata generation")?;
    let chunk_count = usize::try_from(sql_nonnegative_integer(&row[1], "metadata chunk count")?)
        .map_err(|_| VfsError::eio("actor SQLite VFS metadata chunk count overflow"))?;
    let byte_length = usize::try_from(sql_nonnegative_integer(&row[2], "metadata byte length")?)
        .map_err(|_| VfsError::eio("actor SQLite VFS metadata byte length overflow"))?;
    if byte_length > max_metadata_bytes {
        return Err(VfsError::new(
            "EFBIG",
            format!(
                "stored actor SQLite VFS metadata is {byte_length} bytes, exceeding maxMetadataBytes={max_metadata_bytes}; raise chunked_actor_sqlite.maxMetadataBytes"
            ),
        ));
    }
    let expected_chunks = byte_length.div_ceil(METADATA_CHUNK_SIZE);
    if chunk_count != expected_chunks {
        return Err(VfsError::eio(format!(
            "actor SQLite VFS metadata head has {chunk_count} chunks for {byte_length} bytes; expected {expected_chunks}"
        )));
    }

    let mut dump = Vec::with_capacity(byte_length);
    for chunk_index in 0..chunk_count {
        let result = database
            .query(SqlStatement::new(
                "SELECT content FROM agentos_fs_metadata_chunks WHERE namespace = ? AND generation = ? AND chunk_index = ?",
                vec![
                    SqlValue::SqlText(namespace.to_owned()),
                    SqlValue::SqlInteger(generation),
                    SqlValue::SqlInteger(i64::try_from(chunk_index).map_err(|_| {
                        VfsError::eio("actor SQLite VFS metadata chunk index overflow")
                    })?),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        let content = first_blob(result)?.ok_or_else(|| {
            VfsError::eio(format!(
                "actor SQLite VFS metadata generation {generation} is missing chunk {chunk_index}"
            ))
        })?;
        if content.len() > METADATA_CHUNK_SIZE {
            return Err(VfsError::eio(format!(
                "actor SQLite VFS metadata chunk {chunk_index} is {} bytes, exceeding {METADATA_CHUNK_SIZE}",
                content.len()
            )));
        }
        dump.extend_from_slice(&content);
    }
    if dump.len() != byte_length {
        return Err(VfsError::eio(format!(
            "actor SQLite VFS metadata decoded to {} bytes; expected {byte_length}",
            dump.len()
        )));
    }
    Ok(Some(dump))
}

async fn cleanup_old_metadata(
    database: &SharedVmSqliteDatabase,
    namespace: &str,
    current_generation: i64,
) -> VfsResult<()> {
    loop {
        let result = database
            .query(SqlStatement::new(
                "SELECT generation, chunk_index FROM agentos_fs_metadata_chunks WHERE namespace = ? AND generation <> ? LIMIT ?",
                vec![
                    SqlValue::SqlText(namespace.to_owned()),
                    SqlValue::SqlInteger(current_generation),
                    SqlValue::SqlInteger(METADATA_CLEANUP_BATCH_SIZE),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        if result.rows.is_empty() {
            return Ok(());
        }
        for row in result.rows {
            if row.len() != 2 {
                return Err(VfsError::eio(
                    "actor SQLite returned malformed stale metadata row",
                ));
            }
            let generation = sql_nonnegative_integer(&row[0], "metadata generation")?;
            let chunk_index = sql_nonnegative_integer(&row[1], "metadata chunk index")?;
            database
                .query(SqlStatement::new(
                    "DELETE FROM agentos_fs_metadata_chunks WHERE namespace = ? AND generation = ? AND chunk_index = ?",
                    vec![
                        SqlValue::SqlText(namespace.to_owned()),
                        SqlValue::SqlInteger(generation),
                        SqlValue::SqlInteger(chunk_index),
                    ],
                ))
                .await
                .map_err(actor_sql_error)?;
        }
    }
}

fn first_integer(result: QueryResult, description: &str) -> VfsResult<i64> {
    let row = result
        .rows
        .into_iter()
        .next()
        .ok_or_else(|| VfsError::eio(format!("actor SQLite returned no {description}")))?;
    let value = row
        .first()
        .ok_or_else(|| VfsError::eio(format!("actor SQLite returned empty {description} row")))?;
    sql_nonnegative_integer(value, description)
}

fn sql_nonnegative_integer(value: &SqlValue, description: &str) -> VfsResult<i64> {
    match value {
        SqlValue::SqlInteger(value) if *value >= 0 => Ok(*value),
        _ => Err(VfsError::eio(format!(
            "actor SQLite returned invalid {description}: {value:?}"
        ))),
    }
}

#[async_trait]
impl MetadataStore for ActorSqliteMetadataStore {
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta> {
        self.inner().await?.resolve(path).await
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)> {
        self.inner().await?.resolve_parent(path).await
    }

    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta> {
        self.inner().await?.lstat(path).await
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>> {
        self.inner().await?.list_dir(ino).await
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner.create(parent, name, attrs).await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        inner.link(parent, name, target).await?;
        self.persist(inner).await
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner.remove(parent, name).await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner.rename(src_parent, src, dst_parent, dst).await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner.set_attr(ino, patch).await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner
            .commit_write(ino, edits, new_size, allocated_extents)
            .await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>> {
        self.inner().await?.get_chunks(ino, range).await
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        self.inner().await?.snapshot(root).await
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        let _guard = self.mutation.lock().await;
        let inner = self.inner().await?;
        let result = inner.fork(snap).await?;
        self.persist(inner).await?;
        Ok(result)
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        self.inner().await?.gc().await
    }
}

struct ActorSqliteBlockStore {
    database: SharedVmSqliteDatabase,
    namespace: String,
}

impl ActorSqliteBlockStore {
    fn new(database: SharedVmSqliteDatabase, namespace: String) -> Self {
        Self {
            database,
            namespace,
        }
    }
}

#[async_trait]
impl BlockStore for ActorSqliteBlockStore {
    async fn get(&self, key: &BlockKey) -> VfsResult<Vec<u8>> {
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT content FROM agentos_fs_blocks WHERE namespace = ? AND block_key = ?",
                vec![
                    SqlValue::SqlText(self.namespace.clone()),
                    SqlValue::SqlText(key.0.clone()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        first_blob(result)?.ok_or_else(|| VfsError::enoent(&key.0))
    }

    async fn get_range(&self, key: &BlockKey, off: u64, len: u64) -> VfsResult<Vec<u8>> {
        let offset = i64::try_from(off)
            .map_err(|_| VfsError::einval(format!("block range offset is too large: {off}")))?;
        let length = i64::try_from(len)
            .map_err(|_| VfsError::einval(format!("block range length is too large: {len}")))?;
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT substr(content, ?, ?) FROM agentos_fs_blocks \
                 WHERE namespace = ? AND block_key = ?",
                vec![
                    SqlValue::SqlInteger(offset.saturating_add(1)),
                    SqlValue::SqlInteger(length),
                    SqlValue::SqlText(self.namespace.clone()),
                    SqlValue::SqlText(key.0.clone()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        first_blob(result)?.ok_or_else(|| VfsError::enoent(&key.0))
    }

    async fn put(&self, key: &BlockKey, data: &[u8]) -> VfsResult<()> {
        self.database
            .query(SqlStatement::new(
                "INSERT INTO agentos_fs_blocks (namespace, block_key, content) VALUES (?, ?, ?) \
                 ON CONFLICT(namespace, block_key) DO UPDATE SET content = excluded.content",
                vec![
                    SqlValue::SqlText(self.namespace.clone()),
                    SqlValue::SqlText(key.0.clone()),
                    SqlValue::SqlBlob(data.to_vec()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        Ok(())
    }

    async fn exists(&self, key: &BlockKey) -> VfsResult<bool> {
        let result = self
            .database
            .query(SqlStatement::new(
                "SELECT 1 FROM agentos_fs_blocks WHERE namespace = ? AND block_key = ? LIMIT 1",
                vec![
                    SqlValue::SqlText(self.namespace.clone()),
                    SqlValue::SqlText(key.0.clone()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        Ok(!result.rows.is_empty())
    }

    async fn delete_many(&self, keys: &[BlockKey]) -> VfsResult<()> {
        if keys.is_empty() {
            return Ok(());
        }
        self.database
            .transaction(
                keys.iter()
                    .map(|key| {
                        SqlStatement::new(
                            "DELETE FROM agentos_fs_blocks WHERE namespace = ? AND block_key = ?",
                            vec![
                                SqlValue::SqlText(self.namespace.clone()),
                                SqlValue::SqlText(key.0.clone()),
                            ],
                        )
                    })
                    .collect(),
            )
            .await
            .map_err(actor_sql_error)?;
        Ok(())
    }

    async fn copy(&self, src: &BlockKey, dst: &BlockKey) -> VfsResult<()> {
        let result = self
            .database
            .query(SqlStatement::new(
                "INSERT INTO agentos_fs_blocks (namespace, block_key, content) \
                 SELECT namespace, ?, content FROM agentos_fs_blocks \
                 WHERE namespace = ? AND block_key = ? \
                 ON CONFLICT(namespace, block_key) DO UPDATE SET content = excluded.content",
                vec![
                    SqlValue::SqlText(dst.0.clone()),
                    SqlValue::SqlText(self.namespace.clone()),
                    SqlValue::SqlText(src.0.clone()),
                ],
            ))
            .await
            .map_err(actor_sql_error)?;
        if result.changes == 0 {
            return Err(VfsError::enoent(&src.0));
        }
        Ok(())
    }
}

fn first_blob(result: QueryResult) -> VfsResult<Option<Vec<u8>>> {
    let Some(row) = result.rows.into_iter().next() else {
        return Ok(None);
    };
    match row.into_iter().next() {
        Some(SqlValue::SqlBlob(bytes)) => Ok(Some(bytes)),
        Some(SqlValue::SqlNull) | None => Ok(None),
        Some(value) => Err(VfsError::eio(format!(
            "actor SQLite returned non-BLOB value: {value:?}"
        ))),
    }
}

fn actor_sql_error(error: impl std::fmt::Display) -> VfsError {
    VfsError::eio(format!("actor SQLite UDS: {error}"))
}
