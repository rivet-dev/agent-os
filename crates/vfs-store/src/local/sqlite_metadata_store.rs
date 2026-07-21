use async_trait::async_trait;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use vfs::engine::error::{VfsError, VfsResult};
use vfs::engine::mem::metadata_store::MetadataDump;
use vfs::engine::mem::InMemoryMetadataStore;
use vfs::engine::metadata::MetadataStore;
use vfs::engine::types::{
    BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs, DentryStat, InodeMeta, InodePatch,
    InodeType, SnapshotId, Storage, Timespec, DEFAULT_CHUNK_SIZE,
};

const LOCAL_FS_SCHEMA_VERSION_TABLE: &str = "agentos_fs_schema_version";

struct LocalFsMigration {
    version: i64,
    statements: &'static str,
}

// This ladder belongs to the standalone rusqlite metadata database opened by
// `SqliteMetadataStore`. It is not interchangeable with the filesystem ladder
// installed in the per-VM descriptor database by `chunked_actor_sqlite`.
const LOCAL_FS_MIGRATIONS: &[LocalFsMigration] = &[
    LocalFsMigration {
        version: 1,
        statements: r#"
        CREATE TABLE agentos_fs_inodes (
          ino INTEGER PRIMARY KEY CHECK (ino > 0),
          kind INTEGER NOT NULL CHECK (kind IN (0, 1, 2)),
          mode INTEGER NOT NULL CHECK (mode BETWEEN 0 AND 4294967295),
          uid INTEGER NOT NULL CHECK (uid BETWEEN 0 AND 4294967295),
          gid INTEGER NOT NULL CHECK (gid BETWEEN 0 AND 4294967295),
          size INTEGER NOT NULL CHECK (size >= 0),
          nlink INTEGER NOT NULL CHECK (nlink >= 0),
          atime_ns INTEGER NOT NULL,
          mtime_ns INTEGER NOT NULL,
          ctime_ns INTEGER NOT NULL,
          birthtime_ns INTEGER NOT NULL,
          storage_mode INTEGER NOT NULL CHECK (storage_mode IN (0, 1, 2)),
          storage_chunk_size INTEGER CHECK (
            storage_chunk_size IS NULL OR
            storage_chunk_size BETWEEN 1 AND 4294967295
          ),
          inline_content BLOB,
          symlink_target TEXT,
          CHECK (
            (storage_mode = 0 AND storage_chunk_size IS NULL AND inline_content IS NULL) OR
            (storage_mode = 1 AND storage_chunk_size IS NULL AND inline_content IS NOT NULL) OR
            (storage_mode = 2 AND storage_chunk_size IS NOT NULL AND inline_content IS NULL)
          ),
          CHECK (
            (kind = 2 AND symlink_target IS NOT NULL) OR
            (kind <> 2 AND symlink_target IS NULL)
          )
        ) STRICT;
        CREATE TABLE agentos_fs_dentries (
          parent_ino INTEGER NOT NULL CHECK (parent_ino > 0),
          name TEXT NOT NULL CHECK (length(name) > 0),
          child_ino INTEGER NOT NULL CHECK (child_ino > 0),
          kind INTEGER NOT NULL CHECK (kind IN (0, 1, 2)),
          PRIMARY KEY (parent_ino, name)
        ) STRICT;
        CREATE INDEX agentos_fs_dentries_parent
          ON agentos_fs_dentries(parent_ino);
        CREATE TABLE agentos_fs_chunks (
          ino INTEGER NOT NULL CHECK (ino > 0),
          chunk_index INTEGER NOT NULL CHECK (chunk_index >= 0),
          block_key TEXT NOT NULL CHECK (length(block_key) > 0),
          len INTEGER NOT NULL CHECK (len BETWEEN 0 AND 4294967295),
          PRIMARY KEY (ino, chunk_index)
        ) STRICT;
        CREATE TABLE agentos_fs_block_refs (
          block_key TEXT PRIMARY KEY CHECK (length(block_key) > 0),
          refcount INTEGER NOT NULL CHECK (refcount > 0)
        ) STRICT;
        CREATE TABLE agentos_fs_snapshots (
          snapshot_id INTEGER PRIMARY KEY CHECK (snapshot_id > 0),
          root_ino INTEGER NOT NULL CHECK (root_ino > 0),
          created_ns INTEGER NOT NULL
        ) STRICT;
        "#,
    },
    LocalFsMigration {
        version: 2,
        statements: r#"
            ALTER TABLE agentos_fs_inodes RENAME TO agentos_fs_inodes_v1;
            CREATE TABLE agentos_fs_inodes (
              ino INTEGER PRIMARY KEY CHECK (ino > 0),
              kind INTEGER NOT NULL CHECK (kind IN (0, 1, 2, 3, 4, 5)),
              mode INTEGER NOT NULL CHECK (mode BETWEEN 0 AND 4294967295),
              uid INTEGER NOT NULL CHECK (uid BETWEEN 0 AND 4294967295),
              gid INTEGER NOT NULL CHECK (gid BETWEEN 0 AND 4294967295),
              size INTEGER NOT NULL CHECK (size >= 0),
              nlink INTEGER NOT NULL CHECK (nlink >= 0),
              atime_ns INTEGER NOT NULL,
              mtime_ns INTEGER NOT NULL,
              ctime_ns INTEGER NOT NULL,
              birthtime_ns INTEGER NOT NULL,
              storage_mode INTEGER NOT NULL CHECK (storage_mode IN (0, 1, 2)),
              storage_chunk_size INTEGER CHECK (
                storage_chunk_size IS NULL OR
                storage_chunk_size BETWEEN 1 AND 4294967295
              ),
              inline_content BLOB,
              symlink_target TEXT,
              xattrs_json BLOB NOT NULL DEFAULT X'7B7D',
              allocated_extents_json BLOB NOT NULL DEFAULT X'5B5D',
              CHECK (
                (storage_mode = 0 AND storage_chunk_size IS NULL AND inline_content IS NULL) OR
                (storage_mode = 1 AND storage_chunk_size IS NULL AND inline_content IS NOT NULL) OR
                (storage_mode = 2 AND storage_chunk_size IS NOT NULL AND inline_content IS NULL)
              ),
              CHECK (
                (kind = 2 AND symlink_target IS NOT NULL) OR
                (kind <> 2 AND symlink_target IS NULL)
              )
            ) STRICT;
            INSERT INTO agentos_fs_inodes
              (ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns,
               birthtime_ns, storage_mode, storage_chunk_size, inline_content, symlink_target,
               xattrs_json, allocated_extents_json)
            SELECT ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns,
                   birthtime_ns, storage_mode, storage_chunk_size, inline_content, symlink_target,
                   X'7B7D',
                   CASE WHEN kind = 0 AND size > 0
                     THEN CAST(printf('[[0,%d]]', (size + 511) / 512) AS BLOB)
                     ELSE X'5B5D'
                   END
              FROM agentos_fs_inodes_v1;
            DROP TABLE agentos_fs_inodes_v1;
        "#,
    },
    LocalFsMigration {
        version: 3,
        statements: r#"
            DROP INDEX agentos_fs_dentries_parent;
            ALTER TABLE agentos_fs_dentries RENAME TO agentos_fs_dentries_v2;
            CREATE TABLE agentos_fs_dentries (
              parent_ino INTEGER NOT NULL CHECK (parent_ino > 0),
              name TEXT NOT NULL CHECK (length(name) > 0),
              child_ino INTEGER NOT NULL CHECK (child_ino > 0),
              kind INTEGER NOT NULL CHECK (kind IN (0, 1, 2, 3, 4, 5)),
              PRIMARY KEY (parent_ino, name)
            ) STRICT;
            INSERT INTO agentos_fs_dentries (parent_ino, name, child_ino, kind)
              SELECT parent_ino, name, child_ino, kind
                FROM agentos_fs_dentries_v2;
            DROP TABLE agentos_fs_dentries_v2;
            CREATE INDEX agentos_fs_dentries_parent
              ON agentos_fs_dentries(parent_ino);
        "#,
    },
];

pub struct SqliteMetadataStore {
    connection: Mutex<Connection>,
    pending_write_count: Mutex<usize>,
    inner: InMemoryMetadataStore,
}

const MAX_PENDING_WRITE_COMMITS: usize = 256;

impl SqliteMetadataStore {
    pub fn open(path: impl AsRef<Path>) -> VfsResult<Self> {
        let connection = Connection::open(path)
            .map_err(|err| VfsError::eio(format!("open SQLite metadata store: {err}")))?;
        Self::from_connection(connection)
    }

    pub fn in_memory() -> VfsResult<Self> {
        let connection = Connection::open_in_memory()
            .map_err(|err| VfsError::eio(format!("open in-memory SQLite metadata store: {err}")))?;
        Self::from_connection(connection)
    }

    fn from_connection(mut connection: Connection) -> VfsResult<Self> {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|err| VfsError::eio(format!("enable SQLite WAL mode: {err}")))?;
        connection
            .pragma_update(None, "synchronous", "NORMAL")
            .map_err(|err| VfsError::eio(format!("configure SQLite synchronous mode: {err}")))?;
        install_schema(&mut connection)?;
        let dump = load_dump(&connection)?;
        let is_new = dump.is_none();
        let inner = dump
            .map(InMemoryMetadataStore::from_dump)
            .unwrap_or_default();
        if is_new {
            persist_dump(&mut connection, &inner.dump())?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
            pending_write_count: Mutex::new(0),
            inner,
        })
    }

    pub fn has_schema(&self) -> VfsResult<bool> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('agentos_fs_inodes', 'agentos_fs_dentries', 'agentos_fs_chunks', 'agentos_fs_block_refs', 'agentos_fs_snapshots')",
                [],
                |row| row.get(0),
            )
            .map_err(|err| VfsError::eio(format!("inspect SQLite schema: {err}")))?;
        Ok(count == 5)
    }

    fn persist(&self) -> VfsResult<()> {
        self.flush_pending_writes()?;
        let dump = self.inner.dump();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        persist_dump(&mut connection, &dump)
    }

    fn persist_create(&self, parent: u64, name: &str, meta: &InodeMeta) -> VfsResult<()> {
        self.flush_pending_writes()?;
        let parent_meta = self.inner.inode_meta(parent)?;
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = connection
            .transaction()
            .map_err(|err| VfsError::eio(format!("begin SQLite create transaction: {err}")))?;
        upsert_inode(&tx, &parent_meta)?;
        upsert_inode(&tx, meta)?;
        tx.execute(
            "INSERT INTO agentos_fs_dentries (parent_ino, name, child_ino, kind) VALUES (?, ?, ?, ?)",
            params![parent, name, meta.ino, kind_id(meta.kind)],
        )
        .map_err(|err| VfsError::eio(format!("persist SQLite dentry {name}: {err}")))?;
        tx.commit()
            .map_err(|err| VfsError::eio(format!("commit SQLite create transaction: {err}")))
    }

    fn persist_set_attr(&self, ino: u64, storage_changed: bool) -> VfsResult<()> {
        let meta = self.inner.inode_meta(ino)?;
        let mut pending = self
            .pending_write_count
            .lock()
            .expect("sqlite pending-write mutex poisoned");
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        if *pending > 0 {
            if let Err(error) = self.persist_set_attr_rows(&connection, &meta, storage_changed) {
                let rollback_result = connection.execute_batch("ROLLBACK");
                *pending = 0;
                if let Err(rollback_error) = rollback_result {
                    return Err(VfsError::eio(format!(
                        "{error}; rollback batched SQLite setattr failed: {rollback_error}"
                    )));
                }
                return Err(error);
            }
            return Ok(());
        }

        let tx = connection
            .transaction()
            .map_err(|err| VfsError::eio(format!("begin SQLite setattr transaction: {err}")))?;
        self.persist_set_attr_rows(&tx, &meta, storage_changed)?;
        tx.commit()
            .map_err(|err| VfsError::eio(format!("commit SQLite setattr transaction: {err}")))
    }

    fn persist_set_attr_rows(
        &self,
        connection: &Connection,
        meta: &InodeMeta,
        storage_changed: bool,
    ) -> VfsResult<()> {
        let mut affected_keys = BTreeSet::new();
        if storage_changed {
            let mut statement = connection
                .prepare_cached("SELECT block_key FROM agentos_fs_chunks WHERE ino = ?")
                .map_err(|err| VfsError::eio(format!("prepare setattr chunk lookup: {err}")))?;
            let rows = statement
                .query_map(params![meta.ino], |row| row.get::<_, String>(0))
                .map_err(|err| VfsError::eio(format!("query setattr chunks: {err}")))?;
            for row in rows {
                affected_keys.insert(BlockKey(
                    row.map_err(|err| VfsError::eio(format!("read setattr chunk key: {err}")))?,
                ));
            }
        }
        upsert_inode(connection, meta)?;
        if storage_changed {
            connection
                .execute(
                    "DELETE FROM agentos_fs_chunks WHERE ino = ?",
                    params![meta.ino],
                )
                .map_err(|err| VfsError::eio(format!("delete setattr chunks: {err}")))?;
            for key in affected_keys {
                let refcount = self.inner.refcount(&key);
                if refcount == 0 {
                    connection
                        .execute(
                            "DELETE FROM agentos_fs_block_refs WHERE block_key = ?",
                            params![key.0],
                        )
                        .map_err(|err| {
                            VfsError::eio(format!("delete setattr block ref {}: {err}", key.0))
                        })?;
                } else {
                    connection
                        .execute(
                            "INSERT INTO agentos_fs_block_refs (block_key, refcount) VALUES (?, ?)
                             ON CONFLICT(block_key) DO UPDATE SET refcount=excluded.refcount",
                            params![key.0, refcount],
                        )
                        .map_err(|err| {
                            VfsError::eio(format!("persist setattr block ref {}: {err}", key.0))
                        })?;
                }
            }
        }
        Ok(())
    }

    fn flush_pending_writes(&self) -> VfsResult<()> {
        let mut pending = self
            .pending_write_count
            .lock()
            .expect("sqlite pending-write mutex poisoned");
        if *pending == 0 {
            return Ok(());
        }
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .execute_batch("COMMIT")
            .map_err(|err| VfsError::eio(format!("commit pending SQLite writes: {err}")))?;
        *pending = 0;
        Ok(())
    }

    fn flush_durable(&self) -> VfsResult<()> {
        self.flush_pending_writes()?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .execute_batch("PRAGMA wal_checkpoint(FULL)")
            .map_err(|err| VfsError::eio(format!("checkpoint SQLite metadata WAL: {err}")))
    }

    fn persist_commit_write(
        &self,
        ino: u64,
        edits: &[ChunkEdit],
        old_size: u64,
        new_size: u64,
        chunk_size: u64,
    ) -> VfsResult<()> {
        let meta = self.inner.inode_meta(ino)?;
        let keep_chunks = if new_size == 0 {
            0
        } else {
            new_size.div_ceil(chunk_size)
        };
        let old_chunks = if old_size == 0 {
            0
        } else {
            old_size.div_ceil(chunk_size)
        };
        let mut pending = self
            .pending_write_count
            .lock()
            .expect("sqlite pending-write mutex poisoned");
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        if *pending == 0 {
            connection.execute_batch("BEGIN IMMEDIATE").map_err(|err| {
                VfsError::eio(format!("begin batched SQLite write transaction: {err}"))
            })?;
        }

        let write_result = (|| -> VfsResult<()> {
            let mut affected_keys = BTreeSet::new();
            upsert_inode(&connection, &meta)?;
            if new_size < old_size {
                let mut statement = connection
                    .prepare_cached(
                        "SELECT block_key FROM agentos_fs_chunks WHERE ino = ? AND chunk_index >= ?",
                    )
                    .map_err(|err| {
                        VfsError::eio(format!("prepare truncated chunk lookup: {err}"))
                    })?;
                let rows = statement
                    .query_map(params![ino, keep_chunks], |row| row.get::<_, String>(0))
                    .map_err(|err| VfsError::eio(format!("query truncated chunks: {err}")))?;
                for row in rows {
                    affected_keys.insert(BlockKey(row.map_err(|err| {
                        VfsError::eio(format!("read truncated chunk key: {err}"))
                    })?));
                }
            }

            for edit in edits.iter().filter(|edit| edit.index < keep_chunks) {
                let previous = if edit.index >= old_chunks {
                    None
                } else {
                    connection
                        .prepare_cached(
                            "SELECT block_key FROM agentos_fs_chunks WHERE ino = ? AND chunk_index = ?",
                        )
                        .map_err(|err| {
                            VfsError::eio(format!(
                                "prepare previous SQLite chunk lookup {ino}/{}: {err}",
                                edit.index
                            ))
                        })?
                        .query_row(params![ino, edit.index], |row| row.get::<_, String>(0))
                        .optional()
                        .map_err(|err| {
                            VfsError::eio(format!(
                                "query previous SQLite chunk {ino}/{}: {err}",
                                edit.index
                            ))
                        })?
                };
                if let Some(key) = previous {
                    affected_keys.insert(BlockKey(key));
                }
                affected_keys.insert(edit.key.clone());
            }

            if new_size < old_size {
                connection
                    .prepare_cached(
                        "DELETE FROM agentos_fs_chunks WHERE ino = ? AND chunk_index >= ?",
                    )
                    .map_err(|err| VfsError::eio(format!("prepare truncated chunk delete: {err}")))?
                    .execute(params![ino, keep_chunks])
                    .map_err(|err| {
                        VfsError::eio(format!("delete truncated SQLite chunks: {err}"))
                    })?;
            }
            let mut insert_chunk = connection
                .prepare_cached(
                    "INSERT INTO agentos_fs_chunks (ino, chunk_index, block_key, len) VALUES (?, ?, ?, ?)
                     ON CONFLICT(ino, chunk_index) DO UPDATE SET
                       block_key=excluded.block_key, len=excluded.len",
                )
                .map_err(|err| VfsError::eio(format!("prepare SQLite chunk upsert: {err}")))?;
            for edit in edits.iter().filter(|edit| edit.index < keep_chunks) {
                insert_chunk
                    .execute(params![ino, edit.index, edit.key.0, edit.len])
                    .map_err(|err| {
                        VfsError::eio(format!("persist SQLite chunk {ino}/{}: {err}", edit.index))
                    })?;
            }

            for key in affected_keys {
                let refcount = self.inner.refcount(&key);
                if refcount == 0 {
                    connection
                        .execute(
                            "DELETE FROM agentos_fs_block_refs WHERE block_key = ?",
                            params![key.0],
                        )
                        .map_err(|err| {
                            VfsError::eio(format!("delete SQLite block ref {}: {err}", key.0))
                        })?;
                } else {
                    connection
                        .execute(
                            "INSERT INTO agentos_fs_block_refs (block_key, refcount) VALUES (?, ?)
                             ON CONFLICT(block_key) DO UPDATE SET refcount=excluded.refcount",
                            params![key.0, refcount],
                        )
                        .map_err(|err| {
                            VfsError::eio(format!("persist SQLite block ref {}: {err}", key.0))
                        })?;
                }
            }
            Ok(())
        })();

        if let Err(error) = write_result {
            let rollback_result = connection.execute_batch("ROLLBACK");
            *pending = 0;
            if let Err(rollback_error) = rollback_result {
                return Err(VfsError::eio(format!(
                    "{error}; rollback batched SQLite writes failed: {rollback_error}"
                )));
            }
            return Err(error);
        }

        *pending += 1;
        if *pending >= MAX_PENDING_WRITE_COMMITS {
            connection.execute_batch("COMMIT").map_err(|err| {
                VfsError::eio(format!("commit bounded SQLite write batch: {err}"))
            })?;
            *pending = 0;
        }
        Ok(())
    }
}

impl Drop for SqliteMetadataStore {
    fn drop(&mut self) {
        if let Err(error) = self.flush_pending_writes() {
            eprintln!("failed to flush pending SQLite metadata writes during drop: {error}");
        }
    }
}

fn upsert_inode(connection: &Connection, meta: &InodeMeta) -> VfsResult<()> {
    let (storage_mode, storage_chunk_size, inline_content) = match &meta.storage {
        Storage::None => (0, None, None),
        Storage::Inline(data) => (1, None, Some(data.as_slice())),
        Storage::Chunked { chunk_size } => (2, Some(*chunk_size), None),
    };
    let xattrs_json = serde_json::to_vec(&meta.xattrs)
        .map_err(|err| VfsError::eio(format!("serialize inode {} xattrs: {err}", meta.ino)))?;
    let allocated_extents_json = serde_json::to_vec(&meta.allocated_extents).map_err(|err| {
        VfsError::eio(format!(
            "serialize inode {} allocation extents: {err}",
            meta.ino
        ))
    })?;
    connection
        .execute(
            "INSERT INTO agentos_fs_inodes
             (ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns, birthtime_ns,
              storage_mode, storage_chunk_size, inline_content, symlink_target, xattrs_json,
              allocated_extents_json)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(ino) DO UPDATE SET
              kind=excluded.kind, mode=excluded.mode, uid=excluded.uid, gid=excluded.gid,
              size=excluded.size, nlink=excluded.nlink, atime_ns=excluded.atime_ns,
              mtime_ns=excluded.mtime_ns, ctime_ns=excluded.ctime_ns,
              birthtime_ns=excluded.birthtime_ns, storage_mode=excluded.storage_mode,
              storage_chunk_size=excluded.storage_chunk_size,
              inline_content=excluded.inline_content, symlink_target=excluded.symlink_target,
              xattrs_json=excluded.xattrs_json,
              allocated_extents_json=excluded.allocated_extents_json",
            params![
                meta.ino,
                kind_id(meta.kind),
                meta.mode,
                meta.uid,
                meta.gid,
                meta.size,
                meta.nlink,
                timespec_to_ns(meta.atime),
                timespec_to_ns(meta.mtime),
                timespec_to_ns(meta.ctime),
                timespec_to_ns(meta.birthtime),
                storage_mode,
                storage_chunk_size,
                inline_content,
                meta.symlink_target,
                xattrs_json,
                allocated_extents_json,
            ],
        )
        .map_err(|err| VfsError::eio(format!("persist SQLite inode {}: {err}", meta.ino)))?;
    Ok(())
}

fn install_schema(connection: &mut Connection) -> VfsResult<()> {
    install_schema_migrations(connection, LOCAL_FS_MIGRATIONS)
}

fn install_schema_migrations(
    connection: &mut Connection,
    migrations: &[LocalFsMigration],
) -> VfsResult<()> {
    validate_migration_ladder(migrations)?;
    let latest_version = migrations.last().map_or(0, |migration| migration.version);
    let tx = connection
        .transaction()
        .map_err(|err| VfsError::eio(format!("begin SQLite schema migration: {err}")))?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS agentos_fs_schema_version (
           singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
           schema_version INTEGER NOT NULL CHECK (schema_version >= 0)
         ) STRICT;",
    )
    .map_err(|err| VfsError::eio(format!("install SQLite schema version table: {err}")))?;

    let row_count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM agentos_fs_schema_version",
            [],
            |row| row.get(0),
        )
        .map_err(|err| VfsError::eio(format!("inspect SQLite schema version rows: {err}")))?;
    let current_version = match row_count {
        0 => 0,
        1 => tx
            .query_row(
                "SELECT schema_version FROM agentos_fs_schema_version WHERE singleton = 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|err| VfsError::eio(format!("read SQLite schema version: {err}")))?,
        count => {
            return Err(VfsError::eio(format!(
                "{LOCAL_FS_SCHEMA_VERSION_TABLE} must contain at most one row; found {count}"
            )))
        }
    };
    if !(0..=latest_version).contains(&current_version) {
        return Err(VfsError::eio(format!(
            "unsupported {LOCAL_FS_SCHEMA_VERSION_TABLE} version {current_version}; latest supported version is {latest_version}"
        )));
    }

    for migration in migrations
        .iter()
        .filter(|migration| migration.version > current_version)
    {
        tx.execute_batch(migration.statements).map_err(|err| {
            VfsError::eio(format!(
                "apply SQLite filesystem migration {}: {err}",
                migration.version
            ))
        })?;
        tx.execute(
            "INSERT INTO agentos_fs_schema_version (singleton, schema_version)
             VALUES (1, ?1)
             ON CONFLICT(singleton) DO UPDATE SET schema_version = excluded.schema_version",
            [migration.version],
        )
        .map_err(|err| {
            VfsError::eio(format!(
                "record SQLite filesystem migration {}: {err}",
                migration.version
            ))
        })?;
    }

    tx.commit()
        .map_err(|err| VfsError::eio(format!("commit SQLite schema migration: {err}")))
}

fn validate_migration_ladder(migrations: &[LocalFsMigration]) -> VfsResult<()> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = i64::try_from(index + 1)
            .map_err(|_| VfsError::eio("SQLite filesystem migration version overflow"))?;
        if migration.version != expected {
            return Err(VfsError::eio(format!(
                "malformed SQLite filesystem migration ladder: expected version {expected}, found {}",
                migration.version
            )));
        }
        if migration.statements.trim().is_empty() {
            return Err(VfsError::eio(format!(
                "malformed SQLite filesystem migration ladder: version {expected} has no statements"
            )));
        }
    }
    Ok(())
}

fn load_dump(connection: &Connection) -> VfsResult<Option<MetadataDump>> {
    let inode_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM agentos_fs_inodes", [], |row| {
            row.get(0)
        })
        .map_err(|err| VfsError::eio(format!("count SQLite inodes: {err}")))?;
    if inode_count == 0 {
        return Ok(None);
    }

    let mut inodes = BTreeMap::new();
    let mut next_ino = 1;
    let mut statement = connection
        .prepare(
            "SELECT ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns,
                    birthtime_ns, storage_mode, storage_chunk_size, inline_content, symlink_target,
                    xattrs_json, allocated_extents_json
             FROM agentos_fs_inodes",
        )
        .map_err(|err| VfsError::eio(format!("prepare inode load: {err}")))?;
    let rows = statement
        .query_map([], |row| {
            let ino: u64 = row.get(0)?;
            let kind_id: i64 = row.get(1)?;
            let storage_id: i64 = row.get(11)?;
            let chunk_size: Option<u32> = row.get(12)?;
            let inline_content: Option<Vec<u8>> = row.get(13)?;
            let symlink_target: Option<String> = row.get(14)?;
            let xattrs_json: Vec<u8> = row.get(15)?;
            let allocated_extents_json: Vec<u8> = row.get(16)?;
            let kind = match kind_id {
                0 => InodeType::File,
                1 => InodeType::Directory,
                2 => InodeType::Symlink,
                3 => InodeType::CharacterDevice,
                4 => InodeType::BlockDevice,
                _ => InodeType::Fifo,
            };
            let storage = match storage_id {
                1 => Storage::Inline(inline_content.unwrap_or_default()),
                2 => Storage::Chunked {
                    chunk_size: chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE),
                },
                _ => Storage::None,
            };
            Ok(InodeMeta {
                ino,
                kind,
                mode: row.get(2)?,
                uid: row.get(3)?,
                gid: row.get(4)?,
                size: row.get(5)?,
                nlink: row.get(6)?,
                atime: ns_to_timespec(row.get(7)?),
                mtime: ns_to_timespec(row.get(8)?),
                ctime: ns_to_timespec(row.get(9)?),
                birthtime: ns_to_timespec(row.get(10)?),
                storage,
                symlink_target,
                allocated_extents: serde_json::from_slice(&allocated_extents_json).map_err(
                    |error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            allocated_extents_json.len(),
                            rusqlite::types::Type::Blob,
                            Box::new(error),
                        )
                    },
                )?,
                xattrs: serde_json::from_slice(&xattrs_json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        xattrs_json.len(),
                        rusqlite::types::Type::Blob,
                        Box::new(error),
                    )
                })?,
            })
        })
        .map_err(|err| VfsError::eio(format!("load SQLite inodes: {err}")))?;
    for row in rows {
        let meta = row.map_err(|err| VfsError::eio(format!("load SQLite inode row: {err}")))?;
        next_ino = next_ino.max(meta.ino + 1);
        inodes.insert(meta.ino, meta);
    }

    let mut dentries = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT parent_ino, name, child_ino FROM agentos_fs_dentries")
        .map_err(|err| VfsError::eio(format!("prepare dentry load: {err}")))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                (row.get::<_, u64>(0)?, row.get::<_, String>(1)?),
                row.get::<_, u64>(2)?,
            ))
        })
        .map_err(|err| VfsError::eio(format!("load SQLite dentries: {err}")))?;
    for row in rows {
        let (key, value) =
            row.map_err(|err| VfsError::eio(format!("load SQLite dentry row: {err}")))?;
        dentries.insert(key, value);
    }

    let mut chunks = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT ino, chunk_index, block_key, len FROM agentos_fs_chunks")
        .map_err(|err| VfsError::eio(format!("prepare chunk load: {err}")))?;
    let rows = statement
        .query_map([], |row| {
            let index = row.get::<_, u64>(1)?;
            Ok((
                (row.get::<_, u64>(0)?, index),
                ChunkRef {
                    index,
                    key: BlockKey(row.get(2)?),
                    len: row.get(3)?,
                },
            ))
        })
        .map_err(|err| VfsError::eio(format!("load SQLite chunks: {err}")))?;
    for row in rows {
        let (key, value) =
            row.map_err(|err| VfsError::eio(format!("load SQLite chunk row: {err}")))?;
        chunks.insert(key, value);
    }

    let mut block_refs = BTreeMap::new();
    let mut statement = connection
        .prepare("SELECT block_key, refcount FROM agentos_fs_block_refs")
        .map_err(|err| VfsError::eio(format!("prepare block ref load: {err}")))?;
    let rows = statement
        .query_map([], |row| Ok((BlockKey(row.get(0)?), row.get::<_, u64>(1)?)))
        .map_err(|err| VfsError::eio(format!("load SQLite block refs: {err}")))?;
    for row in rows {
        let (key, value) =
            row.map_err(|err| VfsError::eio(format!("load SQLite block ref row: {err}")))?;
        block_refs.insert(key, value);
    }

    Ok(Some(MetadataDump {
        next_ino,
        inodes,
        dentries,
        chunks,
        block_refs,
    }))
}

fn persist_dump(connection: &mut Connection, dump: &MetadataDump) -> VfsResult<()> {
    let tx = connection
        .transaction()
        .map_err(|err| VfsError::eio(format!("begin SQLite metadata transaction: {err}")))?;
    tx.execute_batch(
        "
        DELETE FROM agentos_fs_snapshots;
        DELETE FROM agentos_fs_block_refs;
        DELETE FROM agentos_fs_chunks;
        DELETE FROM agentos_fs_dentries;
        DELETE FROM agentos_fs_inodes;
        ",
    )
    .map_err(|err| VfsError::eio(format!("clear SQLite metadata tables: {err}")))?;

    for meta in dump.inodes.values() {
        upsert_inode(&tx, meta)?;
    }

    for ((parent, name), child) in &dump.dentries {
        let kind = dump
            .inodes
            .get(child)
            .map(|meta| meta.kind)
            .ok_or_else(|| VfsError::eio(format!("dentry points to missing inode {child}")))?;
        tx.execute(
            "INSERT INTO agentos_fs_dentries (parent_ino, name, child_ino, kind) VALUES (?, ?, ?, ?)",
            params![parent, name, child, kind_id(kind)],
        )
        .map_err(|err| VfsError::eio(format!("persist SQLite dentry {name}: {err}")))?;
    }

    for ((ino, index), chunk) in &dump.chunks {
        tx.execute(
            "INSERT INTO agentos_fs_chunks (ino, chunk_index, block_key, len) VALUES (?, ?, ?, ?)",
            params![ino, index, chunk.key.0, chunk.len],
        )
        .map_err(|err| VfsError::eio(format!("persist SQLite chunk {ino}/{index}: {err}")))?;
    }

    for (key, refcount) in &dump.block_refs {
        tx.execute(
            "INSERT INTO agentos_fs_block_refs (block_key, refcount) VALUES (?, ?)",
            params![key.0, refcount],
        )
        .map_err(|err| VfsError::eio(format!("persist SQLite block ref {}: {err}", key.0)))?;
    }

    tx.commit()
        .map_err(|err| VfsError::eio(format!("commit SQLite metadata transaction: {err}")))
}

fn kind_id(kind: InodeType) -> i64 {
    match kind {
        InodeType::File => 0,
        InodeType::Directory => 1,
        InodeType::Symlink => 2,
        InodeType::CharacterDevice => 3,
        InodeType::BlockDevice => 4,
        InodeType::Fifo => 5,
    }
}

fn timespec_to_ns(time: Timespec) -> i64 {
    time.sec.saturating_mul(1_000_000_000) + i64::from(time.nsec)
}

fn ns_to_timespec(ns: i64) -> Timespec {
    Timespec {
        sec: ns / 1_000_000_000,
        nsec: ns.rem_euclid(1_000_000_000) as u32,
    }
}

#[async_trait]
impl MetadataStore for SqliteMetadataStore {
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta> {
        self.inner.resolve(path).await
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)> {
        self.inner.resolve_parent(path).await
    }

    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta> {
        self.inner.lstat(path).await
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>> {
        self.inner.list_dir(ino).await
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta> {
        let result = self.inner.create(parent, name, attrs).await;
        if let Ok(meta) = &result {
            self.persist_create(parent, name, meta)?;
        }
        result
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        let result = self.inner.link(parent, name, target).await;
        if result.is_ok() {
            self.persist()?;
        }
        result
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        let result = self.inner.remove(parent, name).await;
        if result.is_ok() {
            self.persist()?;
        }
        result
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        let result = self.inner.rename(src_parent, src, dst_parent, dst).await;
        if result.is_ok() {
            self.persist()?;
        }
        result
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        let storage_changed = patch.storage.is_some();
        let result = self.inner.set_attr(ino, patch).await;
        if result.is_ok() {
            self.persist_set_attr(ino, storage_changed)?;
        }
        result
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        let chunk_size = match self.inner.inode_meta(ino)?.storage {
            Storage::Chunked { chunk_size } => u64::from(chunk_size),
            Storage::Inline(_) | Storage::None => u64::from(DEFAULT_CHUNK_SIZE),
        };
        let old_size = self.inner.inode_meta(ino)?.size;
        let persisted_edits = edits.clone();
        let result = self
            .inner
            .commit_write(ino, edits, new_size, allocated_extents)
            .await;
        if result.is_ok() {
            self.persist_commit_write(ino, &persisted_edits, old_size, new_size, chunk_size)?;
        }
        result
    }

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>> {
        self.inner.get_chunks(ino, range).await
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        self.inner.snapshot(root).await
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        let result = self.inner.fork(snap).await;
        if result.is_ok() {
            self.persist()?;
        }
        result
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        self.inner.gc().await
    }

    async fn flush(&self) -> VfsResult<()> {
        self.flush_durable()
    }
}

#[cfg(test)]
mod writeback_tests {
    use super::*;

    #[test]
    fn file_store_uses_writeback_sqlite_settings() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteMetadataStore::open(temp.path().join("metadata.sqlite")).unwrap();
        let connection = store.connection.lock().expect("sqlite mutex poisoned");
        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_malformed_ladder_before_touching_database() {
        const MALFORMED: &[LocalFsMigration] = &[LocalFsMigration {
            version: 2,
            statements: "CREATE TABLE agentos_fs_probe (value INTEGER) STRICT;",
        }];
        let mut connection = Connection::open_in_memory().expect("open database");

        let error = install_schema_migrations(&mut connection, MALFORMED)
            .expect_err("malformed ladder must fail");

        assert!(error.message().contains("expected version 1, found 2"));
        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name LIKE 'agentos_fs_%'",
                [],
                |row| row.get(0),
            )
            .expect("inspect database");
        assert_eq!(table_count, 0);
    }

    #[test]
    fn rolls_back_schema_and_version_when_migration_fails() {
        const FAILING: &[LocalFsMigration] = &[LocalFsMigration {
            version: 1,
            statements: "CREATE TABLE agentos_fs_probe (value INTEGER CHECK (value > 0)) STRICT;
                         INSERT INTO agentos_fs_probe (value) VALUES (0);",
        }];
        let mut connection = Connection::open_in_memory().expect("open database");

        install_schema_migrations(&mut connection, FAILING)
            .expect_err("failing migration must roll back");

        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('agentos_fs_schema_version', 'agentos_fs_probe')",
                [],
                |row| row.get(0),
            )
            .expect("inspect database");
        assert_eq!(table_count, 0);
    }
}
