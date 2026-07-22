use async_trait::async_trait;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::sleep;
use std::time::Duration;
use vfs::engine::engines::{ChunkedFs, ChunkedFsOptions, ObjectFs};
use vfs::engine::mem::{InMemoryMetadataStore, MemoryBlockStore, MemoryObjectBackend};
use vfs::engine::{
    BlockKey, CachedMetadataStore, ChunkEdit, ChunkRange, CreateInodeAttrs, FileExtent, InodePatch,
    InodeType, MetadataStore, ObjectBackend, SnapshotId, Storage, VfsResult, VirtualFileSystem,
    S_IFBLK, S_IFIFO,
};

#[tokio::test]
async fn chunked_fs_updates_content_metadata_and_namespace_timestamps() {
    let fs = ChunkedFs::new(InMemoryMetadataStore::new(), MemoryBlockStore::new());
    fs.create_dir("/dir").await.unwrap();
    let parent_before_create = fs.stat("/dir").await.unwrap();

    sleep(Duration::from_millis(2));
    fs.write_file("/dir/file", b"first").await.unwrap();
    let parent_after_create = fs.stat("/dir").await.unwrap();
    assert!(parent_after_create.mtime > parent_before_create.mtime);
    assert!(parent_after_create.ctime > parent_before_create.ctime);

    let before_write = fs.stat("/dir/file").await.unwrap();
    sleep(Duration::from_millis(2));
    fs.write_file("/dir/file", b"second").await.unwrap();
    let after_write = fs.stat("/dir/file").await.unwrap();
    assert_eq!(after_write.atime, before_write.atime);
    assert!(after_write.mtime > before_write.mtime);
    assert!(after_write.ctime > before_write.ctime);

    sleep(Duration::from_millis(2));
    fs.chmod("/dir/file", 0o600).await.unwrap();
    let after_chmod = fs.stat("/dir/file").await.unwrap();
    assert_eq!(after_chmod.atime, after_write.atime);
    assert_eq!(after_chmod.mtime, after_write.mtime);
    assert!(after_chmod.ctime > after_write.ctime);

    sleep(Duration::from_millis(2));
    fs.rename("/dir/file", "/dir/renamed").await.unwrap();
    let after_rename = fs.stat("/dir/renamed").await.unwrap();
    assert_eq!(after_rename.atime, after_chmod.atime);
    assert_eq!(after_rename.mtime, after_chmod.mtime);
    assert!(after_rename.ctime > after_chmod.ctime);
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_atime_update_preserves_content_and_other_timestamps() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/file", b"whole object").await.unwrap();
    let before = fs.stat("/file").await.unwrap();
    let atime_ms = u64::try_from(before.atime.sec).unwrap() * 1_000
        + u64::from(before.atime.nsec / 1_000_000)
        + 2_000;

    fs.set_atime("/file", atime_ms).await.unwrap();

    let after = fs.stat("/file").await.unwrap();
    assert!(after.atime > before.atime);
    assert_eq!(after.mtime, before.mtime);
    assert_eq!(after.ctime, before.ctime);
    assert_eq!(after.birthtime, before.birthtime);
    assert_eq!(fs.read_file("/file").await.unwrap(), b"whole object");
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_coalesces_dirty_state_until_sync() {
    let backend = MemoryObjectBackend::new();
    let fs = ObjectFs::new(backend.clone());

    fs.write_file("/file", b"initial").await.unwrap();
    fs.pwrite("/file", b"final", 0).await.unwrap();
    fs.truncate("/file", 5).await.unwrap();
    fs.chmod("/file", 0o600).await.unwrap();

    assert!(backend.head("file").await.unwrap().is_none());
    assert_eq!(fs.read_file("/file").await.unwrap(), b"final");
    assert_eq!(fs.stat("/file").await.unwrap().mode & 0o777, 0o600);

    fs.sync("/file").await.unwrap();

    let persisted = backend.head("file").await.unwrap().unwrap();
    assert_eq!(persisted.mode & 0o777, 0o600);
    assert_eq!(
        backend.get_range("file", 0, persisted.size).await.unwrap(),
        b"final"
    );
}

#[tokio::test]
async fn chunked_fs_preserves_special_inode_metadata() {
    let fs = ChunkedFs::new(InMemoryMetadataStore::new(), MemoryBlockStore::new());
    fs.create_dir("/devices").await.unwrap();
    fs.mknod("/devices/block", S_IFBLK | 0o640, (8 << 8) | 1)
        .await
        .unwrap();
    fs.mknod("/devices/fifo", S_IFIFO | 0o600, 0).await.unwrap();
    fs.set_xattr("/devices/fifo", "user.probe", b"fifo", 0, true)
        .await
        .unwrap();

    let block = fs.stat("/devices/block").await.unwrap();
    assert_eq!(block.mode & 0o170000, S_IFBLK);
    assert_eq!(block.rdev, (8 << 8) | 1);
    assert!(fs
        .list_xattrs("/devices/block", true)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fs.get_xattr("/devices/block", "agentos.internal.rdev", true)
            .await
            .unwrap_err()
            .code(),
        "EOPNOTSUPP"
    );
    let entries = fs.read_dir_with_types("/devices").await.unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry.name == "block" && entry.kind == InodeType::BlockDevice));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "fifo" && entry.kind == InodeType::Fifo));
    assert_eq!(
        fs.get_xattr("/devices/fifo", "user.probe", true)
            .await
            .unwrap(),
        b"fifo"
    );
    assert_eq!(
        fs.read_file("/devices/fifo").await.unwrap_err().code(),
        "ENXIO"
    );
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_preserves_special_inode_metadata() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.create_dir("/devices").await.unwrap();
    fs.mknod("/devices/block", S_IFBLK | 0o640, (8 << 8) | 1)
        .await
        .unwrap();
    fs.mknod("/devices/fifo", S_IFIFO | 0o600, 0).await.unwrap();
    fs.set_xattr("/devices/fifo", "user.probe", b"fifo", 0, true)
        .await
        .unwrap();

    let block = fs.stat("/devices/block").await.unwrap();
    assert_eq!(block.mode & 0o170000, S_IFBLK);
    assert_eq!(block.rdev, (8 << 8) | 1);
    assert!(fs
        .list_xattrs("/devices/block", true)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        fs.get_xattr("/devices/block", "agentos.internal.rdev", true)
            .await
            .unwrap_err()
            .code(),
        "EOPNOTSUPP"
    );
    let entries = fs.read_dir_with_types("/devices").await.unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry.name == "block" && entry.kind == InodeType::BlockDevice));
    assert!(entries
        .iter()
        .any(|entry| entry.name == "fifo" && entry.kind == InodeType::Fifo));
    assert_eq!(
        fs.get_xattr("/devices/fifo", "user.probe", true)
            .await
            .unwrap(),
        b"fifo"
    );
    assert_eq!(
        fs.read_file("/devices/fifo").await.unwrap_err().code(),
        "ENXIO"
    );
}

#[tokio::test]
async fn chunked_fs_round_trips_inline_and_chunked_files() {
    let metadata = InMemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let fs = ChunkedFs::with_options(
        metadata,
        blocks.clone(),
        ChunkedFsOptions {
            inline_threshold: 4,
            chunk_size: 3,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/small.txt", b"abc").await.unwrap();
    assert_eq!(fs.read_file("/small.txt").await.unwrap(), b"abc");
    assert_eq!(blocks.len(), 0);

    fs.write_file("/large.txt", b"abcdefghi").await.unwrap();
    assert_eq!(fs.read_file("/large.txt").await.unwrap(), b"abcdefghi");
    assert_eq!(blocks.len(), 3);

    fs.pwrite("/large.txt", b"ZZ", 2).await.unwrap();
    assert_eq!(fs.read_file("/large.txt").await.unwrap(), b"abZZefghi");
}

#[tokio::test]
async fn adaptive_chunking_uses_first_large_write_as_the_inode_chunk_size() {
    let metadata = InMemoryMetadataStore::new();
    let fs = ChunkedFs::with_adaptive_chunk_size(
        metadata,
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/adaptive", b"abcdefghij").await.unwrap();
    let meta = fs.metadata().resolve("/adaptive").await.unwrap();
    assert_eq!(meta.storage, Storage::Chunked { chunk_size: 10 });
    assert_eq!(
        fs.metadata()
            .get_chunks(
                meta.ino,
                ChunkRange {
                    start: 0,
                    end: None,
                },
            )
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn chunked_fs_xattrs_are_inode_scoped() {
    let fs = ChunkedFs::new(InMemoryMetadataStore::new(), MemoryBlockStore::new());
    fs.write_file("/file", b"data").await.unwrap();
    fs.link("/file", "/hardlink").await.unwrap();
    fs.set_xattr("/file", "user.agentos", b"value", 1, true)
        .await
        .unwrap();
    assert_eq!(
        fs.get_xattr("/hardlink", "user.agentos", true)
            .await
            .unwrap(),
        b"value"
    );
    assert_eq!(
        fs.list_xattrs("/file", true).await.unwrap(),
        vec!["user.agentos"]
    );
}

#[tokio::test]
async fn chunked_fs_partial_writes_preserve_untouched_chunks() {
    let metadata = InMemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let fs = ChunkedFs::with_options(
        metadata.clone(),
        blocks,
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/large.txt", b"aaaabbbbcccc").await.unwrap();
    let ino = metadata.resolve("/large.txt").await.unwrap().ino;
    let before = metadata.get_chunks(ino, ChunkRange::all()).await.unwrap();
    assert_eq!(before.len(), 3);

    fs.pwrite("/large.txt", b"Z", 5).await.unwrap();

    assert_eq!(fs.read_file("/large.txt").await.unwrap(), b"aaaabZbbcccc");
    let after = metadata.get_chunks(ino, ChunkRange::all()).await.unwrap();
    assert_eq!(after.len(), 3);
    assert_eq!(after[0].key, before[0].key);
    assert_ne!(after[1].key, before[1].key);
    assert_eq!(after[2].key, before[2].key);
}

#[tokio::test]
async fn chunked_fs_sparse_pwrite_and_truncate_zero_fill_holes() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 4,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/sparse", b"").await.unwrap();
    fs.pwrite("/sparse", b"end", 10).await.unwrap();
    assert_eq!(
        fs.read_file("/sparse").await.unwrap(),
        b"\0\0\0\0\0\0\0\0\0\0end"
    );
    let sparse_stat = fs.stat("/sparse").await.unwrap();
    assert_eq!(sparse_stat.size, 13);
    assert_eq!(sparse_stat.blocks, 1);

    fs.write_file("/truncate", b"abcdefgh").await.unwrap();
    fs.truncate("/truncate", 5).await.unwrap();
    fs.truncate("/truncate", 8).await.unwrap();
    assert_eq!(fs.read_file("/truncate").await.unwrap(), b"abcde\0\0\0");
    assert_eq!(fs.stat("/truncate").await.unwrap().blocks, 1);
}

#[tokio::test]
async fn chunked_fs_allocate_preserves_data_and_materializes_sparse_blocks() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 128,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/allocated", b"prefix").await.unwrap();
    fs.allocate("/allocated", 512, 512).await.unwrap();

    let contents = fs.read_file("/allocated").await.unwrap();
    assert_eq!(&contents[..6], b"prefix");
    assert!(contents[6..].iter().all(|byte| *byte == 0));
    let stat = fs.stat("/allocated").await.unwrap();
    assert_eq!(stat.size, 1024);
    assert_eq!(stat.blocks, 2);
}

#[tokio::test]
async fn chunked_fs_punch_hole_preserves_size_and_deallocates_complete_blocks() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 512,
            ..ChunkedFsOptions::default()
        },
    );
    fs.write_file("/punched", &vec![b'x'; 2048]).await.unwrap();

    fs.punch_hole("/punched", 512, 1024).await.unwrap();

    let contents = fs.read_file("/punched").await.unwrap();
    assert!(contents[..512].iter().all(|byte| *byte == b'x'));
    assert!(contents[512..1536].iter().all(|byte| *byte == 0));
    assert!(contents[1536..].iter().all(|byte| *byte == b'x'));
    let stat = fs.stat("/punched").await.unwrap();
    assert_eq!(stat.size, 2048);
    assert_eq!(stat.blocks, 2);
    assert_eq!(
        fs.allocated_ranges("/punched").await.unwrap(),
        vec![(0, 512), (1536, 2048)]
    );
}

#[tokio::test]
async fn chunked_fs_zero_range_reallocates_exact_bytes_and_honors_keep_size() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 512,
            ..ChunkedFsOptions::default()
        },
    );
    fs.write_file("/zeroed", &vec![b'x'; 2048]).await.unwrap();
    fs.punch_hole("/zeroed", 512, 512).await.unwrap();

    fs.zero_range("/zeroed", 384, 768, false).await.unwrap();
    let contents = fs.read_file("/zeroed").await.unwrap();
    assert!(contents[..384].iter().all(|byte| *byte == b'x'));
    assert!(contents[384..1152].iter().all(|byte| *byte == 0));
    assert!(contents[1152..].iter().all(|byte| *byte == b'x'));
    assert_eq!(
        fs.allocated_ranges("/zeroed").await.unwrap(),
        vec![(0, 2048)]
    );
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(512, 1024)]
    );

    fs.zero_range("/zeroed", 3072, 512, true).await.unwrap();
    assert_eq!(fs.stat("/zeroed").await.unwrap().size, 2048);
    fs.truncate("/zeroed", 3584).await.unwrap();
    assert_eq!(
        fs.allocated_ranges("/zeroed").await.unwrap(),
        vec![(0, 2048), (3072, 3584)]
    );
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(512, 1024), (3072, 3584)]
    );
    let mut indexed_extents = Vec::new();
    for index in 0..5 {
        indexed_extents.push(fs.extent_at("/zeroed", index).await.unwrap());
    }
    assert_eq!(
        indexed_extents,
        vec![
            Some(FileExtent {
                start: 0,
                end: 512,
                unwritten: false,
            }),
            Some(FileExtent {
                start: 512,
                end: 1024,
                unwritten: true,
            }),
            Some(FileExtent {
                start: 1024,
                end: 2048,
                unwritten: false,
            }),
            Some(FileExtent {
                start: 3072,
                end: 3584,
                unwritten: true,
            }),
            None,
        ]
    );
    fs.pwrite("/zeroed", &vec![b'y'; 512], 512).await.unwrap();
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(3072, 3584)]
    );
}

#[tokio::test]
async fn chunked_fs_expansion_preserves_keep_size_allocation_past_new_eof() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 512,
            ..ChunkedFsOptions::default()
        },
    );
    fs.write_file("/reserved", &vec![b'x'; 2048]).await.unwrap();
    fs.zero_range("/reserved", 2048, 2048, true).await.unwrap();
    assert_eq!(fs.stat("/reserved").await.unwrap().size, 2048);
    assert_eq!(
        fs.unwritten_ranges("/reserved").await.unwrap(),
        vec![(2048, 4096)]
    );

    fs.truncate("/reserved", 3072).await.unwrap();
    assert_eq!(fs.stat("/reserved").await.unwrap().size, 3072);
    assert_eq!(
        fs.allocated_ranges("/reserved").await.unwrap(),
        vec![(0, 4096)]
    );
    assert_eq!(
        fs.unwritten_ranges("/reserved").await.unwrap(),
        vec![(2048, 4096)]
    );
}

#[tokio::test]
async fn chunked_fs_same_size_truncate_discards_keep_size_allocation_past_eof() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 512,
            ..ChunkedFsOptions::default()
        },
    );
    fs.write_file("/reserved", &vec![b'x'; 2048]).await.unwrap();
    fs.zero_range("/reserved", 2048, 2048, true).await.unwrap();

    fs.truncate("/reserved", 2048).await.unwrap();

    assert_eq!(
        fs.allocated_ranges("/reserved").await.unwrap(),
        vec![(0, 2048)]
    );
    assert!(fs.unwritten_ranges("/reserved").await.unwrap().is_empty());
}

#[tokio::test]
async fn chunked_fs_dedups_identical_content_and_gc_deletes_on_unlink() {
    let metadata = InMemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let fs = ChunkedFs::with_options(
        metadata.clone(),
        blocks.clone(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 16,
            ..ChunkedFsOptions::default()
        },
    );

    fs.write_file("/a", b"same content").await.unwrap();
    fs.write_file("/b", b"same content").await.unwrap();
    let key = BlockKey::from_content(b"same content");
    assert_eq!(blocks.len(), 1);
    assert_eq!(metadata.refcount(&key), 2);

    fs.remove_file("/a").await.unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(metadata.refcount(&key), 1);

    fs.remove_file("/b").await.unwrap();
    assert_eq!(blocks.len(), 0);
    assert_eq!(metadata.refcount(&key), 0);
}

#[tokio::test]
async fn metadata_snapshot_and_fork_share_chunk_refs_until_cow_write() {
    let metadata = InMemoryMetadataStore::new();
    let parent = metadata.resolve("/").await.unwrap();
    let file = metadata
        .create(
            parent.ino,
            "file",
            CreateInodeAttrs::file(0o644, 0, 0, Storage::Chunked { chunk_size: 16 }),
        )
        .await
        .unwrap();
    let key = BlockKey::from_content(b"hello");
    metadata
        .commit_write(
            file.ino,
            vec![ChunkEdit {
                index: 0,
                key: key.clone(),
                len: 5,
            }],
            5,
            vec![(0, 1)],
        )
        .await
        .unwrap();
    assert_eq!(metadata.refcount(&key), 1);

    let snap = metadata.snapshot(parent.ino).await.unwrap();
    assert_eq!(metadata.refcount(&key), 2);
    let fork_root = metadata.fork(snap).await.unwrap();
    assert_eq!(metadata.refcount(&key), 3);

    let fork_entries = metadata.list_dir(fork_root).await.unwrap();
    let fork_file = fork_entries
        .iter()
        .find(|entry| entry.name == "file")
        .unwrap()
        .meta
        .ino;
    let new_key = BlockKey::from_content(b"goodbye");
    metadata
        .commit_write(
            fork_file,
            vec![ChunkEdit {
                index: 0,
                key: new_key.clone(),
                len: 7,
            }],
            7,
            vec![(0, 1)],
        )
        .await
        .unwrap();
    assert_eq!(metadata.refcount(&key), 2);
    assert_eq!(metadata.refcount(&new_key), 1);
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_empty_root_exists_for_first_file_creation() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());

    assert!(fs.exists("/").await);
    assert!(fs.stat("/").await.unwrap().is_directory);
    assert!(fs.read_dir("/").await.unwrap().is_empty());

    fs.write_file("/first", b"created").await.unwrap();
    assert_eq!(fs.read_file("/first").await.unwrap(), b"created");
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_root_supports_persistent_metadata_and_acl_lookup() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());

    let missing_acl = fs
        .get_xattr("/", "system.posix_acl_access", true)
        .await
        .unwrap_err();
    assert_eq!(missing_acl.code(), "ENODATA");
    fs.chmod("/", 0o770).await.unwrap();
    fs.chown("/", 1000, 1001).await.unwrap();
    fs.set_xattr("/", "user.root", b"value", 1, true)
        .await
        .unwrap();

    let stat = fs.stat("/").await.unwrap();
    assert_eq!(stat.mode & 0o7777, 0o770);
    assert_eq!((stat.uid, stat.gid), (1000, 1001));
    assert_eq!(
        fs.get_xattr("/", "user.root", true).await.unwrap(),
        b"value"
    );
}

#[tokio::test]
async fn object_fs_maps_files_to_native_objects() {
    let backend = MemoryObjectBackend::new();
    let fs = ObjectFs::new(backend);
    fs.mkdir("/dir", false).await.unwrap();
    fs.write_file("/dir/file.txt", b"hello").await.unwrap();

    assert_eq!(fs.read_file("/dir/file.txt").await.unwrap(), b"hello");
    assert_eq!(fs.read_dir("/dir").await.unwrap(), vec!["file.txt"]);
    assert_eq!(fs.pread("/dir/file.txt", 1, 3).await.unwrap(), b"ell");

    let file_entry = fs
        .read_dir_with_types("/dir")
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "file.txt")
        .unwrap();
    let file_stat = fs.stat("/dir/file.txt").await.unwrap();
    assert_ne!(file_entry.ino, 0);
    assert_eq!(file_entry.ino, file_stat.ino);

    let directory_entry = fs
        .read_dir_with_types("/")
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.name == "dir")
        .unwrap();
    let directory_stat = fs.stat("/dir").await.unwrap();
    assert_ne!(directory_entry.ino, 0);
    assert_eq!(directory_entry.ino, directory_stat.ino);
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_metadata_mutations_preserve_file_contents() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/owned", b"contents").await.unwrap();

    fs.chmod("/owned", 0o600).await.unwrap();
    fs.chown("/owned", 1000, 1001).await.unwrap();
    fs.utimes("/owned", 10, 20).await.unwrap();
    fs.write_file("/owned", b"updated").await.unwrap();

    let stat = fs.stat("/owned").await.unwrap();
    assert_eq!(stat.mode & 0o7777, 0o600);
    assert_eq!((stat.uid, stat.gid), (1000, 1001));
    assert_eq!(fs.read_file("/owned").await.unwrap(), b"updated");
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_preserves_sparse_allocation_accounting() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/sparse", b"").await.unwrap();
    fs.pwrite("/sparse", &vec![b'x'; 50 * 1024], 1_600 * 1024)
        .await
        .unwrap();

    let stat = fs.stat("/sparse").await.unwrap();
    assert_eq!(stat.size, 1_650 * 1024);
    assert_eq!(stat.blocks, 100);
    assert!(stat.blocks * 512 < stat.size);

    fs.set_xattr("/sparse", "user.test", b"value", 0, true)
        .await
        .unwrap();
    assert_eq!(fs.stat("/sparse").await.unwrap().blocks, 100);

    fs.truncate("/sparse", 1_610 * 1024).await.unwrap();
    assert_eq!(fs.stat("/sparse").await.unwrap().blocks, 20);
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_whole_object_sparse_mutations_preserve_semantics() {
    const WRITE_OFFSET: u64 = 16 * 1024 * 1024;
    const LOGICAL_SIZE: u64 = 32 * 1024 * 1024;

    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/sparse", b"").await.unwrap();
    fs.pwrite("/sparse", b"data", WRITE_OFFSET).await.unwrap();
    fs.truncate("/sparse", LOGICAL_SIZE).await.unwrap();

    let stat = fs.stat("/sparse").await.unwrap();
    assert_eq!(stat.size, LOGICAL_SIZE);
    assert_eq!(stat.blocks, 1);
    assert_eq!(
        fs.pread("/sparse", WRITE_OFFSET - 4, 12).await.unwrap(),
        b"\0\0\0\0data\0\0\0\0"
    );
    assert_eq!(
        fs.pread("/sparse", LOGICAL_SIZE - 8, 8).await.unwrap(),
        vec![0; 8]
    );
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_punch_hole_preserves_size_and_deallocates_complete_blocks() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/punched", &vec![b'x'; 2048]).await.unwrap();

    fs.punch_hole("/punched", 512, 1024).await.unwrap();

    let contents = fs.read_file("/punched").await.unwrap();
    assert!(contents[..512].iter().all(|byte| *byte == b'x'));
    assert!(contents[512..1536].iter().all(|byte| *byte == 0));
    assert!(contents[1536..].iter().all(|byte| *byte == b'x'));
    let stat = fs.stat("/punched").await.unwrap();
    assert_eq!(stat.size, 2048);
    assert_eq!(stat.blocks, 2);
    assert_eq!(
        fs.allocated_ranges("/punched").await.unwrap(),
        vec![(0, 512), (1536, 2048)]
    );
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_zero_range_reallocates_exact_bytes_and_honors_keep_size() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/zeroed", &vec![b'x'; 2048]).await.unwrap();
    fs.punch_hole("/zeroed", 512, 512).await.unwrap();

    fs.zero_range("/zeroed", 384, 768, false).await.unwrap();
    let contents = fs.read_file("/zeroed").await.unwrap();
    assert!(contents[..384].iter().all(|byte| *byte == b'x'));
    assert!(contents[384..1152].iter().all(|byte| *byte == 0));
    assert!(contents[1152..].iter().all(|byte| *byte == b'x'));
    assert_eq!(
        fs.allocated_ranges("/zeroed").await.unwrap(),
        vec![(0, 2048)]
    );
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(512, 1024)]
    );

    fs.zero_range("/zeroed", 3072, 512, true).await.unwrap();
    assert_eq!(fs.stat("/zeroed").await.unwrap().size, 2048);
    fs.truncate("/zeroed", 3584).await.unwrap();
    assert_eq!(
        fs.allocated_ranges("/zeroed").await.unwrap(),
        vec![(0, 2048), (3072, 3584)]
    );
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(512, 1024), (3072, 3584)]
    );
    fs.pwrite("/zeroed", &vec![b'y'; 512], 512).await.unwrap();
    assert_eq!(
        fs.unwritten_ranges("/zeroed").await.unwrap(),
        vec![(3072, 3584)]
    );
}

#[tokio::test]
async fn chunked_fs_insert_and_collapse_shift_bytes_and_extents() {
    let fs = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions {
            inline_threshold: 1,
            chunk_size: 512,
            ..ChunkedFsOptions::default()
        },
    );
    let data = [
        vec![b'A'; 512],
        vec![b'B'; 512],
        vec![b'C'; 512],
        vec![b'D'; 512],
    ]
    .concat();
    fs.write_file("/shift", &data).await.unwrap();
    fs.punch_hole("/shift", 512, 512).await.unwrap();

    fs.insert_range("/shift", 512, 512).await.unwrap();
    let inserted = fs.read_file("/shift").await.unwrap();
    assert_eq!(inserted.len(), 2560);
    assert_eq!(&inserted[..512], vec![b'A'; 512]);
    assert!(inserted[512..1536].iter().all(|byte| *byte == 0));
    assert_eq!(&inserted[1536..2048], vec![b'C'; 512]);
    assert_eq!(
        fs.allocated_ranges("/shift").await.unwrap(),
        vec![(0, 512), (1536, 2560)]
    );

    fs.collapse_range("/shift", 512, 512).await.unwrap();
    fs.collapse_range("/shift", 512, 512).await.unwrap();
    assert_eq!(
        fs.read_file("/shift").await.unwrap(),
        [vec![b'A'; 512], vec![b'C'; 512], vec![b'D'; 512]].concat()
    );
    assert_eq!(
        fs.allocated_ranges("/shift").await.unwrap(),
        vec![(0, 1536)]
    );
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_insert_and_collapse_shift_bytes_and_extents() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    let data = [
        vec![b'A'; 512],
        vec![b'B'; 512],
        vec![b'C'; 512],
        vec![b'D'; 512],
    ]
    .concat();
    fs.write_file("/shift", &data).await.unwrap();
    fs.punch_hole("/shift", 512, 512).await.unwrap();

    fs.insert_range("/shift", 512, 512).await.unwrap();
    let inserted = fs.read_file("/shift").await.unwrap();
    assert_eq!(inserted.len(), 2560);
    assert_eq!(&inserted[..512], vec![b'A'; 512]);
    assert!(inserted[512..1536].iter().all(|byte| *byte == 0));
    assert_eq!(&inserted[1536..2048], vec![b'C'; 512]);
    assert_eq!(
        fs.allocated_ranges("/shift").await.unwrap(),
        vec![(0, 512), (1536, 2560)]
    );

    fs.collapse_range("/shift", 512, 512).await.unwrap();
    fs.collapse_range("/shift", 512, 512).await.unwrap();
    assert_eq!(
        fs.read_file("/shift").await.unwrap(),
        [vec![b'A'; 512], vec![b'C'; 512], vec![b'D'; 512]].concat()
    );
    assert_eq!(
        fs.allocated_ranges("/shift").await.unwrap(),
        vec![(0, 1536)]
    );
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_realpath_resolves_symlinks_and_rejects_loops() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/implicit/file", b"implicit").await.unwrap();
    assert_eq!(
        fs.realpath("/implicit/file").await.unwrap(),
        "/implicit/file"
    );
    fs.mkdir("/dir", false).await.unwrap();
    fs.write_file("/dir/file", b"contents").await.unwrap();
    fs.symlink("dir/file", "/relative").await.unwrap();
    fs.symlink("/relative", "/absolute").await.unwrap();

    assert_eq!(fs.realpath("/relative").await.unwrap(), "/dir/file");
    assert_eq!(fs.realpath("/absolute").await.unwrap(), "/dir/file");
    assert_eq!(fs.realpath("/dir").await.unwrap(), "/dir");

    fs.symlink("self", "/self").await.unwrap();
    assert_eq!(fs.realpath("/self").await.unwrap_err().code(), "ELOOP");
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_xattrs_follow_create_replace_and_remove_semantics() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/xattrs", b"contents").await.unwrap();

    fs.set_xattr("/xattrs", "user.test", b"one", 1, true)
        .await
        .unwrap();
    assert_eq!(
        fs.get_xattr("/xattrs", "user.test", true).await.unwrap(),
        b"one"
    );
    assert!(fs
        .set_xattr("/xattrs", "user.test", b"again", 1, true)
        .await
        .is_err());
    fs.set_xattr("/xattrs", "user.test", b"two", 2, true)
        .await
        .unwrap();
    assert_eq!(
        fs.list_xattrs("/xattrs", true).await.unwrap(),
        vec!["user.test"]
    );
    fs.remove_xattr("/xattrs", "user.test", true).await.unwrap();
    assert!(fs.get_xattr("/xattrs", "user.test", true).await.is_err());
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_hard_links_share_content_metadata_and_link_count() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());
    fs.write_file("/source", b"first").await.unwrap();
    fs.link("/source", "/alias").await.unwrap();

    let source = fs.stat("/source").await.unwrap();
    let alias = fs.stat("/alias").await.unwrap();
    assert_ne!(source.ino, 0);
    assert_eq!(source.ino, alias.ino);
    assert_eq!((source.nlink, alias.nlink), (2, 2));

    fs.write_file("/alias", b"second").await.unwrap();
    fs.set_xattr("/source", "user.shared", b"yes", 1, true)
        .await
        .unwrap();
    assert_eq!(fs.read_file("/source").await.unwrap(), b"second");
    assert_eq!(
        fs.get_xattr("/alias", "user.shared", true).await.unwrap(),
        b"yes"
    );

    fs.remove_file("/source").await.unwrap();
    assert_eq!(fs.stat("/alias").await.unwrap().nlink, 1);
    assert_eq!(fs.read_file("/alias").await.unwrap(), b"second");
}

#[tokio::test]
async fn object_fs_recursively_renames_prefix_directories() {
    let backend = MemoryObjectBackend::new();
    let fs = ObjectFs::new(backend);
    fs.mkdir("/src/nested", true).await.unwrap();
    fs.write_file("/src/root.txt", b"root").await.unwrap();
    fs.write_file("/src/nested/leaf.txt", b"leaf")
        .await
        .unwrap();

    fs.rename("/src", "/dst").await.unwrap();

    assert!(!fs.exists("/src/root.txt").await);
    assert_eq!(fs.read_file("/dst/root.txt").await.unwrap(), b"root");
    assert_eq!(fs.read_file("/dst/nested/leaf.txt").await.unwrap(), b"leaf");
}

#[tokio::test]
#[ignore = "ObjectS3 is dormant; retain this unsupported whole-object target for its return"]
async fn object_fs_rename_enforces_linux_replacement_rules() {
    let fs = ObjectFs::new(MemoryObjectBackend::new());

    fs.write_file("/file", b"source").await.unwrap();
    fs.mkdir("/directory", false).await.unwrap();
    let error = fs.rename("/file", "/directory").await.unwrap_err();
    assert_eq!(error.code(), "EISDIR");
    assert_eq!(fs.read_file("/file").await.unwrap(), b"source");
    assert!(fs.stat("/directory").await.unwrap().is_directory);

    fs.mkdir("/source-dir", false).await.unwrap();
    fs.write_file("/destination-file", b"destination")
        .await
        .unwrap();
    let error = fs
        .rename("/source-dir", "/destination-file")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "ENOTDIR");
    assert!(fs.stat("/source-dir").await.unwrap().is_directory);
    assert_eq!(
        fs.read_file("/destination-file").await.unwrap(),
        b"destination"
    );

    fs.mkdir("/source-tree", false).await.unwrap();
    fs.write_file("/source-tree/source", b"source")
        .await
        .unwrap();
    fs.mkdir("/destination-tree", false).await.unwrap();
    fs.write_file("/destination-tree/destination", b"destination")
        .await
        .unwrap();
    let error = fs
        .rename("/source-tree", "/destination-tree")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "ENOTEMPTY");
    assert_eq!(
        fs.read_file("/source-tree/source").await.unwrap(),
        b"source"
    );
    assert_eq!(
        fs.read_file("/destination-tree/destination").await.unwrap(),
        b"destination"
    );

    fs.mkdir("/empty-source", false).await.unwrap();
    fs.mkdir("/empty-destination", false).await.unwrap();
    fs.rename("/empty-source", "/empty-destination")
        .await
        .unwrap();
    assert!(!fs.exists("/empty-source").await);
    assert!(fs.stat("/empty-destination").await.unwrap().is_directory);

    fs.symlink("relative-target", "/source-link").await.unwrap();
    fs.write_file("/destination", b"destination").await.unwrap();
    fs.rename("/source-link", "/destination").await.unwrap();
    assert!(!fs.exists("/source-link").await);
    assert_eq!(
        fs.readlink("/destination").await.unwrap(),
        "relative-target"
    );
}

#[tokio::test]
async fn cache_invalidates_on_mutation() {
    let metadata = CachedMetadataStore::new(InMemoryMetadataStore::new(), 16);
    let root = metadata.resolve("/").await.unwrap();
    assert!(metadata.resolve("/new").await.is_err());
    metadata
        .create(root.ino, "new", CreateInodeAttrs::directory(0o755, 0, 0))
        .await
        .unwrap();
    assert_eq!(
        metadata.resolve("/new").await.unwrap().kind,
        InodeType::Directory
    );
}

#[tokio::test]
async fn cache_does_not_turn_non_enoent_errors_into_missing_paths() {
    let inner = InMemoryMetadataStore::new();
    let root = inner.resolve("/").await.unwrap();
    inner
        .create(
            root.ino,
            "loop",
            CreateInodeAttrs::symlink(String::from("/loop"), 0, 0),
        )
        .await
        .unwrap();
    let metadata = CachedMetadataStore::new(inner, 16);

    for _ in 0..2 {
        assert_eq!(metadata.resolve("/loop").await.unwrap_err().code(), "ELOOP");
        assert_eq!(
            metadata.lstat("/loop/child").await.unwrap_err().code(),
            "ELOOP"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cache_does_not_store_stale_read_when_mutation_wins_race() {
    let inner = InMemoryMetadataStore::new();
    let root = inner.resolve("/").await.unwrap();
    inner
        .create(
            root.ino,
            "stale",
            CreateInodeAttrs::file(0o644, 0, 0, Storage::Inline(Vec::new())),
        )
        .await
        .unwrap();
    let gate = Arc::new(RaceGate::default());
    let metadata = Arc::new(CachedMetadataStore::new(
        PausingResolveStore {
            inner,
            path: "/stale".to_string(),
            gate: gate.clone(),
        },
        16,
    ));

    let pending_metadata = metadata.clone();
    let pending = tokio::spawn(async move { pending_metadata.resolve("/stale").await });
    gate.wait_until_paused();
    metadata.remove(root.ino, "stale").await.unwrap();
    gate.release();
    assert!(pending.await.unwrap().is_ok());

    let result = metadata.resolve("/stale").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn metadata_set_attr_drops_chunk_refs_when_file_becomes_inline() {
    let metadata = InMemoryMetadataStore::new();
    let parent = metadata.resolve("/").await.unwrap();
    let file = metadata
        .create(
            parent.ino,
            "file",
            CreateInodeAttrs::file(0o644, 0, 0, Storage::Chunked { chunk_size: 16 }),
        )
        .await
        .unwrap();
    let key = BlockKey::from_content(b"hello");
    metadata
        .commit_write(
            file.ino,
            vec![ChunkEdit {
                index: 0,
                key: key.clone(),
                len: 5,
            }],
            5,
            vec![(0, 1)],
        )
        .await
        .unwrap();

    let freed = metadata
        .set_attr(
            file.ino,
            InodePatch {
                storage: Some(Storage::Inline(b"hi".to_vec())),
                size: Some(2),
                ..InodePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(freed, vec![key]);
    assert!(metadata
        .get_chunks(file.ino, ChunkRange::all())
        .await
        .unwrap()
        .is_empty());
}

#[derive(Debug, Default)]
struct RaceGate {
    state: Mutex<RaceGateState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct RaceGateState {
    paused_once: bool,
    paused: bool,
    released: bool,
}

impl RaceGate {
    fn pause_once(&self) {
        let mut state = self.state.lock().expect("race gate poisoned");
        if state.paused_once {
            return;
        }
        state.paused_once = true;
        state.paused = true;
        self.changed.notify_all();
        while !state.released {
            state = self.changed.wait(state).expect("race gate poisoned");
        }
    }

    fn wait_until_paused(&self) {
        let mut state = self.state.lock().expect("race gate poisoned");
        while !state.paused {
            state = self.changed.wait(state).expect("race gate poisoned");
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().expect("race gate poisoned");
        state.released = true;
        self.changed.notify_all();
    }
}

#[derive(Debug, Clone)]
struct PausingResolveStore {
    inner: InMemoryMetadataStore,
    path: String,
    gate: Arc<RaceGate>,
}

#[async_trait]
impl MetadataStore for PausingResolveStore {
    async fn resolve(&self, path: &str) -> VfsResult<vfs::engine::InodeMeta> {
        let result = self.inner.resolve(path).await;
        if path == self.path {
            self.gate.pause_once();
        }
        result
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(vfs::engine::InodeMeta, String)> {
        self.inner.resolve_parent(path).await
    }

    async fn lstat(&self, path: &str) -> VfsResult<vfs::engine::InodeMeta> {
        self.inner.lstat(path).await
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<vfs::engine::DentryStat>> {
        self.inner.list_dir(ino).await
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<vfs::engine::InodeMeta> {
        self.inner.create(parent, name, attrs).await
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        self.inner.link(parent, name, target).await
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        self.inner.remove(parent, name).await
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        self.inner.rename(src_parent, src, dst_parent, dst).await
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        self.inner.set_attr(ino, patch).await
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        self.inner
            .commit_write(ino, edits, new_size, allocated_extents)
            .await
    }

    async fn get_chunks(
        &self,
        ino: u64,
        range: ChunkRange,
    ) -> VfsResult<Vec<vfs::engine::ChunkRef>> {
        self.inner.get_chunks(ino, range).await
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        self.inner.snapshot(root).await
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        self.inner.fork(snap).await
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        self.inner.gc().await
    }
}
