use agentos_runtime::{RuntimeConfig, SidecarRuntime};
use agentos_vfs::{FileBlockStore, SqliteMetadataStore};
use rusqlite::Connection;
use vfs::adapter::MountedEngineFileSystem;
use vfs::engine::engines::{ChunkedFs, ChunkedFsOptions};
use vfs::engine::mem::MemoryBlockStore;
use vfs::engine::{BlockKey, BlockStore, VirtualFileSystem};
use vfs::posix::MountedFileSystem;
use vfs::posix::{MemoryFileSystem, MountOptions, MountTable};

fn test_runtime_context() -> agentos_runtime::RuntimeContext {
    SidecarRuntime::process(&RuntimeConfig::default())
        .expect("create test runtime")
        .context()
}

#[tokio::test]
async fn file_block_store_persists_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileBlockStore::new(temp.path()).unwrap();
    let key = BlockKey::from_content(b"persistent");
    store.put(&key, b"persistent").await.unwrap();
    assert_eq!(store.get(&key).await.unwrap(), b"persistent");

    let reopened = FileBlockStore::new(temp.path()).unwrap();
    assert_eq!(reopened.get(&key).await.unwrap(), b"persistent");
}

#[tokio::test]
async fn sqlite_store_installs_canonical_schema() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("schema.sqlite");
    let store = SqliteMetadataStore::open(&db).unwrap();
    assert!(store.has_schema().unwrap());
    drop(store);

    let connection = Connection::open(db).unwrap();
    let version: i64 = connection
        .query_row(
            "SELECT schema_version FROM agentos_fs_schema_version WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, 2);

    let mut statement = connection
        .prepare(
            "SELECT name, sql FROM sqlite_schema
             WHERE type = 'table' AND name LIKE 'agentos_fs_%'
             ORDER BY name",
        )
        .unwrap();
    let tables = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        tables
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "agentos_fs_block_refs",
            "agentos_fs_chunks",
            "agentos_fs_dentries",
            "agentos_fs_inodes",
            "agentos_fs_schema_version",
            "agentos_fs_snapshots",
        ]
    );
    assert!(tables
        .iter()
        .all(|(_, sql)| sql.trim_end().ends_with("STRICT")));

    let legacy_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE name IN ('inodes', 'dentries', 'chunks', 'block_refs', 'snapshots',
                            'agentos_schema_versions')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_count, 0);
}

#[test]
fn sqlite_store_strict_types_and_semantic_checks_reject_invalid_rows() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("constraints.sqlite");
    drop(SqliteMetadataStore::open(&db).unwrap());
    let connection = Connection::open(db).unwrap();

    assert!(connection
        .execute(
            "INSERT INTO agentos_fs_snapshots (snapshot_id, root_ino, created_ns)
             VALUES ('not-an-integer', 1, 0)",
            [],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO agentos_fs_snapshots (snapshot_id, root_ino, created_ns)
             VALUES (0, 1, 0)",
            [],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO agentos_fs_inodes
             (ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns,
              birthtime_ns, storage_mode, storage_chunk_size, inline_content, symlink_target)
             VALUES (2, 9, 420, 0, 0, 0, 1, 0, 0, 0, 0, 0, NULL, NULL, NULL)",
            [],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO agentos_fs_inodes
             (ino, kind, mode, uid, gid, size, nlink, atime_ns, mtime_ns, ctime_ns,
              birthtime_ns, storage_mode, storage_chunk_size, inline_content, symlink_target)
             VALUES (2, 0, 420, 0, 0, 0, 1, 0, 0, 0, 0, 2, NULL, NULL, NULL)",
            [],
        )
        .is_err());
}

#[test]
fn sqlite_store_rejects_future_schema_versions() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("future.sqlite");
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE agentos_fs_schema_version (
               singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
               schema_version INTEGER NOT NULL CHECK (schema_version >= 0)
             ) STRICT;
             INSERT INTO agentos_fs_schema_version (singleton, schema_version) VALUES (1, 3);",
        )
        .unwrap();
    drop(connection);

    let error = SqliteMetadataStore::open(db)
        .err()
        .expect("future schema must be rejected");
    assert!(error
        .message()
        .contains("version 3; latest supported version is 2"));
}

#[tokio::test]
async fn sqlite_store_reopens_persisted_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let blocks = MemoryBlockStore::new();

    {
        let metadata = SqliteMetadataStore::open(&db).unwrap();
        let fs = ChunkedFs::with_options(
            metadata,
            blocks.clone(),
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );
        fs.mkdir("/dir", false).await.unwrap();
        fs.write_file("/dir/file", b"persisted").await.unwrap();
        fs.set_xattr("/dir/file", "user.persisted", b"xattr", 0, true)
            .await
            .unwrap();
    }

    let metadata = SqliteMetadataStore::open(&db).unwrap();
    let fs = ChunkedFs::with_options(
        metadata,
        blocks,
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );
    assert_eq!(fs.read_file("/dir/file").await.unwrap(), b"persisted");
    assert_eq!(fs.read_dir("/dir").await.unwrap(), vec!["file"]);
    assert_eq!(
        fs.get_xattr("/dir/file", "user.persisted", true)
            .await
            .unwrap(),
        b"xattr"
    );
}

#[tokio::test]
async fn sqlite_store_reopens_incremental_pwrite_overwrite_and_truncate() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let block_root = temp.path().join("blocks");

    {
        let fs = ChunkedFs::with_options(
            SqliteMetadataStore::open(&db).unwrap(),
            FileBlockStore::new(&block_root).unwrap(),
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );
        fs.write_file("/file", b"abcdefghijkl").await.unwrap();
        fs.pwrite("/file", b"XY", 5).await.unwrap();
        fs.truncate("/file", 8).await.unwrap();
        assert_eq!(fs.read_file("/file").await.unwrap(), b"abcdeXYh");
    }

    let fs = ChunkedFs::with_options(
        SqliteMetadataStore::open(&db).unwrap(),
        FileBlockStore::new(&block_root).unwrap(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );
    assert_eq!(fs.read_file("/file").await.unwrap(), b"abcdeXYh");
    assert_eq!(fs.stat("/file").await.unwrap().size, 8);
}

#[tokio::test]
async fn sqlite_store_preserves_truncated_chunk_prefix_during_sparse_growth() {
    let temp = tempfile::tempdir().unwrap();
    let fs = ChunkedFs::with_options(
        SqliteMetadataStore::open(temp.path().join("metadata.sqlite")).unwrap(),
        FileBlockStore::new(temp.path().join("blocks")).unwrap(),
        ChunkedFsOptions {
            inline_threshold: 4096,
            chunk_size: 65536,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/file", &vec![0x63; 65536]).await.unwrap();
    fs.truncate("/file", 1).await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"c");

    fs.pwrite("/file", &vec![0x41; 65536], 65536).await.unwrap();
    assert_eq!(fs.pread("/file", 0, 1).await.unwrap(), b"c");
    assert_eq!(fs.pread("/file", 65536, 16).await.unwrap(), vec![0x41; 16]);
}

#[tokio::test]
async fn sqlite_store_commits_each_bounded_write_batch_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let block_root = temp.path().join("blocks");
    let fs = ChunkedFs::with_options(
        SqliteMetadataStore::open(&db).unwrap(),
        FileBlockStore::new(&block_root).unwrap(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );
    fs.write_file("/file", b"").await.unwrap();
    for index in 0..255_u64 {
        fs.pwrite("/file", &[index as u8; 4], index * 4)
            .await
            .unwrap();
    }

    let reopen = || {
        ChunkedFs::with_options(
            SqliteMetadataStore::open(&db).unwrap(),
            FileBlockStore::new(&block_root).unwrap(),
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        )
    };
    assert_eq!(reopen().stat("/file").await.unwrap().size, 4);

    fs.pwrite("/file", &[255; 4], 255 * 4).await.unwrap();
    let reopened = reopen();
    let bytes = reopened.read_file("/file").await.unwrap();
    assert_eq!(bytes.len(), 1024);
    assert_eq!(&bytes[..4], &[0; 4]);
    assert_eq!(&bytes[1020..], &[255; 4]);
}

#[tokio::test]
async fn chunked_local_sync_flushes_pending_metadata_for_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let block_root = temp.path().join("blocks");
    let options = ChunkedFsOptions {
        inline_threshold: 1,
        chunk_size: 4,
        ..ChunkedFsOptions::default()
    };

    {
        let fs = ChunkedFs::with_options(
            SqliteMetadataStore::open(&db).unwrap(),
            FileBlockStore::new(&block_root).unwrap(),
            options.clone(),
        );
        fs.write_file("/file", b"abcdefgh").await.unwrap();
    }

    let fs = ChunkedFs::with_options(
        SqliteMetadataStore::open(&db).unwrap(),
        FileBlockStore::new(&block_root).unwrap(),
        options.clone(),
    );
    fs.pwrite("/file", b"ijkl", 8).await.unwrap();

    let reopen = || {
        ChunkedFs::with_options(
            SqliteMetadataStore::open(&db).unwrap(),
            FileBlockStore::new(&block_root).unwrap(),
            options.clone(),
        )
    };
    assert_eq!(reopen().stat("/file").await.unwrap().size, 8);

    fs.sync("/file").await.unwrap();
    let reopened = reopen();
    assert_eq!(reopened.stat("/file").await.unwrap().size, 12);
    assert_eq!(reopened.read_file("/file").await.unwrap(), b"abcdefghijkl");
}

#[tokio::test]
async fn sqlite_store_persists_sparse_allocation_extents() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let block_root = temp.path().join("blocks");

    {
        let fs = ChunkedFs::with_options(
            SqliteMetadataStore::open(&db).unwrap(),
            FileBlockStore::new(&block_root).unwrap(),
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4096,
                ..ChunkedFsOptions::default()
            },
        );
        fs.write_file("/sparse", b"").await.unwrap();
        fs.pwrite("/sparse", &vec![b'x'; 50 * 1024], 1_600 * 1024)
            .await
            .unwrap();
        assert_eq!(fs.stat("/sparse").await.unwrap().blocks, 100);
    }

    let fs = ChunkedFs::with_options(
        SqliteMetadataStore::open(&db).unwrap(),
        FileBlockStore::new(&block_root).unwrap(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4096,
            ..ChunkedFsOptions::default()
        },
    );
    let stat = fs.stat("/sparse").await.unwrap();
    assert_eq!(stat.size, 1_650 * 1024);
    assert_eq!(stat.blocks, 100);
}

#[tokio::test]
async fn sqlite_store_reopens_many_incremental_creates() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");

    {
        let fs = ChunkedFs::new(
            SqliteMetadataStore::open(&db).unwrap(),
            MemoryBlockStore::new(),
        );
        fs.mkdir("/many", false).await.unwrap();
        for index in 0..1_000 {
            fs.write_file(&format!("/many/file-{index}"), b"")
                .await
                .unwrap();
        }
    }

    let fs = ChunkedFs::new(
        SqliteMetadataStore::open(&db).unwrap(),
        MemoryBlockStore::new(),
    );
    assert_eq!(fs.read_dir("/many").await.unwrap().len(), 1_000);
}

#[tokio::test]
async fn chunked_local_reopens_and_cleans_stale_blocks() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("metadata.sqlite");
    let block_root = temp.path().join("blocks");
    let stale_key = BlockKey::from_content(b"efgh");

    {
        let metadata = SqliteMetadataStore::open(&db).unwrap();
        let blocks = FileBlockStore::new(&block_root).unwrap();
        let fs = ChunkedFs::with_options(
            metadata,
            blocks,
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );
        fs.write_file("/file", b"abcdefgh").await.unwrap();
    }

    let metadata = SqliteMetadataStore::open(&db).unwrap();
    let blocks = FileBlockStore::new(&block_root).unwrap();
    let fs = ChunkedFs::with_options(
        metadata,
        blocks.clone(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );

    assert_eq!(fs.read_file("/file").await.unwrap(), b"abcdefgh");
    fs.truncate("/file", 5).await.unwrap();
    assert_eq!(fs.read_file("/file").await.unwrap(), b"abcde");
    assert!(!blocks.exists(&stale_key).await.unwrap());
}

#[test]
fn chunked_local_mounted_adapter_creates_exclusive_files_with_modes() {
    let temp = tempfile::tempdir().unwrap();
    let fs = ChunkedFs::new(
        SqliteMetadataStore::open(temp.path().join("metadata.sqlite")).unwrap(),
        FileBlockStore::new(temp.path().join("blocks")).unwrap(),
    );
    let mut mounted = MountedEngineFileSystem::with_runtime_context(fs, test_runtime_context());

    mounted.mkdir("/nested", false).unwrap();
    mounted
        .create_file_exclusive_with_mode("/at-root", Vec::new(), Some(0o600))
        .unwrap();
    mounted
        .create_file_exclusive_with_mode("/nested/file", Vec::new(), Some(0o640))
        .unwrap();

    assert_eq!(mounted.stat("/at-root").unwrap().mode & 0o777, 0o600);
    assert_eq!(mounted.stat("/nested/file").unwrap().mode & 0o777, 0o640);
}

#[test]
fn chunked_local_mounted_adapter_preserves_sparse_pwrite_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let fs = ChunkedFs::new(
        SqliteMetadataStore::open(temp.path().join("metadata.sqlite")).unwrap(),
        FileBlockStore::new(temp.path().join("blocks")).unwrap(),
    );
    let mut mounted = MountedEngineFileSystem::with_runtime_context(fs, test_runtime_context());

    mounted.write_file("/sparse", Vec::new()).unwrap();
    mounted
        .pwrite("/sparse", vec![b'x'; 50 * 1024], 1_600 * 1024)
        .unwrap();

    let stat = mounted.stat("/sparse").unwrap();
    assert_eq!(stat.size, 1_650 * 1024);
    assert_eq!(stat.blocks, 100);
}

#[test]
fn chunked_local_mount_table_preserves_sparse_pwrite_allocation() {
    let temp = tempfile::tempdir().unwrap();
    let fs = ChunkedFs::new(
        SqliteMetadataStore::open(temp.path().join("metadata.sqlite")).unwrap(),
        FileBlockStore::new(temp.path().join("blocks")).unwrap(),
    );
    let mounted = MountedEngineFileSystem::with_runtime_context(fs, test_runtime_context());
    let mut table = MountTable::new(MemoryFileSystem::new());
    vfs::posix::VirtualFileSystem::mkdir(&mut table, "/mnt", false).unwrap();
    table
        .mount_boxed(
            "/mnt",
            Box::new(mounted),
            MountOptions::new("chunked_local"),
        )
        .unwrap();

    vfs::posix::VirtualFileSystem::write_file(&mut table, "/mnt/sparse", Vec::new()).unwrap();
    vfs::posix::VirtualFileSystem::pwrite(
        &mut table,
        "/mnt/sparse",
        vec![b'x'; 50 * 1024],
        1_600 * 1024,
    )
    .unwrap();

    let stat = vfs::posix::VirtualFileSystem::stat(&mut table, "/mnt/sparse").unwrap();
    assert_eq!(stat.size, 1_650 * 1024);
    assert_eq!(stat.blocks, 100);
}
