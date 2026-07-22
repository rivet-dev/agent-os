use std::{fmt::Debug, thread::sleep, time::Duration};
use vfs::posix::{
    normalize_path, validate_path, FileExtent, MemoryFileSystem, VfsResult, VirtualFileSystem,
    VirtualTimeSpec, VirtualUtimeSpec, AGENTOS_TIMESTAMP_MAX_SECONDS, RENAME_EXCHANGE,
    RENAME_NOREPLACE, S_IFLNK, S_IFREG, XATTR_CREATE, XATTR_REPLACE,
};

fn assert_error_code<T: Debug>(result: vfs::posix::VfsResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn timestamps_reject_pre_epoch_values_and_clamp_after_2106() {
    assert_error_code(
        VirtualTimeSpec::new(-1, 0).unwrap().to_truncated_millis(),
        "EINVAL",
    );

    let beyond = VirtualTimeSpec::new(AGENTOS_TIMESTAMP_MAX_SECONDS + 1, 123_000_000).unwrap();
    assert_eq!(
        beyond.to_truncated_millis().unwrap(),
        AGENTOS_TIMESTAMP_MAX_SECONDS as u64 * 1_000 + 123
    );

    let mut filesystem = MemoryFileSystem::new();
    filesystem.write_file("/time", b"").unwrap();
    filesystem
        .utimes_spec(
            "/time",
            VirtualUtimeSpec::Set(beyond),
            VirtualUtimeSpec::Set(beyond),
            true,
        )
        .unwrap();
    let stat = filesystem.stat("/time").unwrap();
    assert_eq!(
        stat.mtime_ms,
        AGENTOS_TIMESTAMP_MAX_SECONDS as u64 * 1_000 + 123
    );
}

#[test]
fn character_device_identity_survives_rename_and_snapshot() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem.mkdir("/devices", false).unwrap();
    filesystem
        .mknod("/devices/null", 0o020666, (1 << 8) | 3)
        .unwrap();
    filesystem
        .mknod("/devices/block", 0o060640, (8 << 8) | 1)
        .unwrap();
    filesystem.mknod("/devices/fifo", 0o010600, 0).unwrap();

    let created = filesystem.lstat("/devices/null").unwrap();
    assert_eq!(created.mode & 0o170000, 0o020000);
    assert_eq!(created.rdev, (1 << 8) | 3);

    filesystem.rename("/devices/null", "/devices/sink").unwrap();
    let restored = MemoryFileSystem::from_snapshot(filesystem.snapshot());
    let restored_stat = restored.lstat("/devices/sink").unwrap();
    assert_eq!(restored_stat.mode & 0o170000, 0o020000);
    assert_eq!(restored_stat.rdev, (1 << 8) | 3);
    let block = restored.lstat("/devices/block").unwrap();
    assert_eq!(block.mode & 0o170000, 0o060000);
    assert_eq!(block.rdev, (8 << 8) | 1);
    let fifo = restored.lstat("/devices/fifo").unwrap();
    assert_eq!(fifo.mode & 0o170000, 0o010000);
    assert_eq!(fifo.rdev, 0);
}

fn assert_invalid_path_keeps_snapshot<T: Debug>(
    baseline: &MemoryFileSystem,
    path: &str,
    operation: impl FnOnce(&mut MemoryFileSystem, &str) -> VfsResult<T>,
) {
    let mut filesystem = MemoryFileSystem::from_snapshot(baseline.snapshot());
    let before = filesystem.snapshot();
    assert_error_code(operation(&mut filesystem, path), "EINVAL");
    assert_eq!(filesystem.snapshot(), before);
}

fn generated_invalid_path(seed: u32) -> String {
    let mut path = String::from("/");
    let segments = (seed % 4) + 1;
    for segment in 0..segments {
        if segment > 0 {
            path.push('/');
        }
        path.push(char::from(b'a' + ((seed + segment) % 26) as u8));
        path.push('\0');
        path.push(char::from(b'a' + (((seed / 3) + segment) % 26) as u8));
    }
    path
}

#[test]
fn write_file_normalizes_paths_and_auto_creates_parents() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("workspace//nested/../nested/hello.txt", "hello world")
        .expect("write file");

    assert!(filesystem.exists("/workspace/nested/hello.txt"));
    assert_eq!(
        filesystem
            .read_text_file("/workspace/nested/hello.txt")
            .expect("read text"),
        "hello world"
    );
    assert_eq!(
        normalize_path("/workspace//nested/../nested/hello.txt"),
        "/workspace/nested/hello.txt"
    );
}

#[test]
fn mkdir_and_remove_dir_enforce_parent_and_emptiness_rules() {
    let mut filesystem = MemoryFileSystem::new();

    assert_error_code(filesystem.create_dir("/missing/child"), "ENOENT");

    filesystem
        .mkdir("/tmp/deep/tree", true)
        .expect("recursive mkdir");
    filesystem
        .remove_dir("/tmp/deep/tree")
        .expect("remove empty dir");
    assert!(!filesystem.exists("/tmp/deep/tree"));

    filesystem
        .write_file("/tmp/nonempty/file.txt", "x")
        .expect("write child");
    assert_error_code(filesystem.remove_dir("/tmp/nonempty"), "ENOTEMPTY");
}

#[test]
fn rename_moves_directory_trees_without_losing_children() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/src/sub/one.txt", "1")
        .expect("write first child");
    filesystem
        .write_file("/src/sub/two.txt", "2")
        .expect("write second child");

    filesystem.rename("/src", "/dst").expect("rename tree");

    assert!(!filesystem.exists("/src"));
    assert_eq!(
        filesystem
            .read_text_file("/dst/sub/one.txt")
            .expect("read renamed child"),
        "1"
    );
    assert_eq!(
        filesystem
            .read_text_file("/dst/sub/two.txt")
            .expect("read renamed second child"),
        "2"
    );
}

#[test]
fn rename_at2_preserves_noreplace_exchange_and_invalid_flag_semantics() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem.write_file("/source", "source").unwrap();
    filesystem
        .write_file("/destination", "destination")
        .unwrap();

    assert_error_code(
        filesystem.rename_at2("/missing", "/destination", RENAME_NOREPLACE),
        "ENOENT",
    );
    assert_eq!(
        filesystem.read_text_file("/destination").unwrap(),
        "destination"
    );

    assert_error_code(
        filesystem.rename_at2("/source", "/destination", RENAME_NOREPLACE),
        "EEXIST",
    );
    assert_eq!(filesystem.read_text_file("/source").unwrap(), "source");
    assert_eq!(
        filesystem.read_text_file("/destination").unwrap(),
        "destination"
    );

    filesystem
        .rename_at2("/source", "/destination", RENAME_EXCHANGE)
        .unwrap();
    assert_eq!(filesystem.read_text_file("/source").unwrap(), "destination");
    assert_eq!(filesystem.read_text_file("/destination").unwrap(), "source");

    assert_error_code(
        filesystem.rename_at2(
            "/source",
            "/destination",
            RENAME_NOREPLACE | RENAME_EXCHANGE,
        ),
        "EINVAL",
    );
    assert_eq!(filesystem.read_text_file("/source").unwrap(), "destination");
    assert_eq!(filesystem.read_text_file("/destination").unwrap(), "source");
}

#[test]
fn rename_rejects_incompatible_destination_types_without_mutation() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem.write_file("/file", "file").unwrap();
    filesystem.mkdir("/directory", false).unwrap();

    assert_error_code(filesystem.rename("/file", "/directory"), "EISDIR");
    assert_eq!(filesystem.read_text_file("/file").unwrap(), "file");
    assert!(filesystem.lstat("/directory").unwrap().is_directory);

    assert_error_code(filesystem.rename("/directory", "/file"), "ENOTDIR");
    assert_eq!(filesystem.read_text_file("/file").unwrap(), "file");
    assert!(filesystem.lstat("/directory").unwrap().is_directory);
}

#[test]
fn symlinks_support_readlink_lstat_realpath_and_dangling_targets() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/real/target.txt", "target")
        .expect("write target");
    filesystem
        .symlink("../real/target.txt", "/alias.txt")
        .expect("create symlink");

    assert_eq!(
        filesystem.read_link("/alias.txt").expect("read link"),
        "../real/target.txt"
    );
    assert_eq!(
        filesystem.realpath("/alias.txt").expect("realpath"),
        "/real/target.txt"
    );
    assert_eq!(
        filesystem
            .read_text_file("/alias.txt")
            .expect("read through symlink"),
        "target"
    );

    let link_stat = filesystem.lstat("/alias.txt").expect("lstat symlink");
    assert!(link_stat.is_symbolic_link);
    assert!(!link_stat.is_directory);
    assert_eq!(link_stat.mode & 0o170000, S_IFLNK);

    let target_stat = filesystem.stat("/alias.txt").expect("stat symlink target");
    assert!(!target_stat.is_symbolic_link);
    assert_eq!(target_stat.mode & 0o170000, S_IFREG);

    filesystem
        .symlink("/missing.txt", "/dangling.txt")
        .expect("create dangling symlink");
    let dangling = filesystem.lstat("/dangling.txt").expect("lstat dangling");
    assert!(dangling.is_symbolic_link);
    assert_error_code(filesystem.stat("/dangling.txt"), "ENOENT");
    assert_error_code(filesystem.read_file("/dangling.txt"), "ENOENT");
}

#[test]
fn readlink_on_regular_file_returns_einval() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/regular.txt", "content")
        .expect("write regular file");

    assert_error_code(filesystem.read_link("/regular.txt"), "EINVAL");
}

#[test]
fn symlink_loops_fail_closed() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .symlink("/loop-b.txt", "/loop-a.txt")
        .expect("create first loop entry");
    filesystem
        .symlink("/loop-a.txt", "/loop-b.txt")
        .expect("create second loop entry");

    assert_error_code(filesystem.read_file("/loop-a.txt"), "ELOOP");
}

#[test]
fn path_validation_rejects_nul_without_mutating_filesystem() {
    let mut baseline = MemoryFileSystem::new();
    baseline
        .write_file("/safe/file.txt", "safe contents")
        .expect("seed file");
    baseline
        .write_file("/safe/source.txt", "source")
        .expect("seed link source");
    baseline
        .symlink("/safe/file.txt", "/safe/link.txt")
        .expect("seed symlink");
    baseline
        .create_dir("/safe/empty")
        .expect("seed removable dir");

    let invalid_paths = ["/bad\0path"];

    for invalid_path in invalid_paths {
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.read_file(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.read_dir(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.read_dir_with_types(path)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.stat(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.realpath(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.read_link(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.lstat(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.write_file(path, "x")
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.create_file_exclusive(path, "x")
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.append_file(path, "x")
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.create_dir(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.mkdir(path, true)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.remove_file(path)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| fs.remove_dir(path));
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.rename(path, "/safe/renamed.txt")
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.rename("/safe/file.txt", path)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.symlink("/safe/file.txt", path)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.link(path, "/safe/linked.txt")
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.link("/safe/source.txt", path)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.chmod(path, 0o600)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.chown(path, 1000, 1000)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.utimes(path, 1_000, 2_000)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.truncate(path, 1)
        });
        assert_invalid_path_keeps_snapshot(&baseline, invalid_path, |fs, path| {
            fs.pread(path, 0, 1)
        });
    }
}

#[test]
fn validate_path_rejects_generated_invalid_inputs() {
    for seed in 0..1_000u32 {
        let invalid_path = generated_invalid_path(seed);
        assert!(invalid_path.contains('\0'));
        assert_error_code(validate_path(&invalid_path), "EINVAL");
    }
}

#[test]
fn path_validation_and_storage_accept_non_nul_ascii_control_bytes() {
    let mut filesystem = MemoryFileSystem::new();

    for path in ["/line\nbreak", "/unit\x1fseparator", "/delete\x7fbyte"] {
        validate_path(path).expect("Linux permits non-NUL control bytes in pathnames");
        filesystem
            .write_file(path, path.as_bytes())
            .expect("store control-byte pathname");
        assert_eq!(
            filesystem.read_file(path).expect("read control-byte path"),
            path.as_bytes()
        );
    }
}

#[test]
fn intermediate_symlink_components_are_resolved_for_reads_writes_and_stats() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/b/existing/file.txt", "target")
        .expect("write canonical file");
    filesystem
        .symlink("/b", "/a")
        .expect("create directory symlink");

    assert_eq!(
        filesystem
            .read_text_file("/a/existing/file.txt")
            .expect("read through intermediate symlink"),
        "target"
    );
    assert!(filesystem.exists("/a/existing/file.txt"));
    assert_eq!(
        filesystem
            .realpath("/a/existing/file.txt")
            .expect("realpath through intermediate symlink"),
        "/b/existing/file.txt"
    );
    assert_eq!(
        filesystem
            .stat("/a/existing/file.txt")
            .expect("stat through intermediate symlink")
            .mode
            & 0o170000,
        S_IFREG
    );

    filesystem
        .write_file("/a/new/nested.txt", "created through alias")
        .expect("write through symlinked parent");
    assert_eq!(
        filesystem
            .read_text_file("/b/new/nested.txt")
            .expect("read canonical created file"),
        "created through alias"
    );
}

#[test]
fn intermediate_symlink_loops_fail_closed() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .symlink("/b", "/a")
        .expect("create first loop entry");
    filesystem
        .symlink("/a", "/b")
        .expect("create second loop entry");

    assert_error_code(filesystem.read_file("/a/file.txt"), "ELOOP");
}

#[test]
fn hard_links_share_inode_data_and_survive_original_removal() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/shared.txt", "hello")
        .expect("write shared file");
    filesystem
        .link("/shared.txt", "/linked.txt")
        .expect("create hard link");

    let before = filesystem.stat("/shared.txt").expect("stat original");
    assert_eq!(before.nlink, 2);

    filesystem
        .write_file("/linked.txt", "updated")
        .expect("write through linked path");
    assert_eq!(
        filesystem
            .read_text_file("/shared.txt")
            .expect("read shared inode"),
        "updated"
    );

    filesystem
        .remove_file("/shared.txt")
        .expect("remove original name");
    assert!(!filesystem.exists("/shared.txt"));
    assert_eq!(
        filesystem
            .read_text_file("/linked.txt")
            .expect("read surviving link"),
        "updated"
    );
    assert_eq!(
        filesystem
            .stat("/linked.txt")
            .expect("stat surviving link")
            .nlink,
        1
    );
}

#[test]
fn chmod_chown_utimes_truncate_and_pread_update_metadata_and_contents() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/meta.txt", "hello")
        .expect("write metadata file");
    filesystem
        .truncate("/meta.txt", 8)
        .expect("truncate metadata file");
    filesystem
        .chmod("/meta.txt", 0o755)
        .expect("chmod metadata file");
    filesystem
        .chown("/meta.txt", 2000, 3000)
        .expect("chown metadata file");
    filesystem
        .utimes("/meta.txt", 1_700_000_000_000, 1_710_000_000_000)
        .expect("utimes metadata file");

    let stat = filesystem.stat("/meta.txt").expect("stat metadata file");
    assert_eq!(stat.mode & 0o170000, S_IFREG);
    assert_eq!(stat.mode & 0o777, 0o755);
    assert_eq!(stat.uid, 2000);
    assert_eq!(stat.gid, 3000);
    assert_eq!(stat.atime_ms, 1_700_000_000_000);
    assert_eq!(stat.mtime_ms, 1_710_000_000_000);
    assert_eq!(stat.size, 8);
    assert_eq!(stat.blocks, 1);
    // Device ids are unique per filesystem instance, so only assert that the
    // value is stable within this filesystem.
    assert_ne!(stat.dev, 0);
    assert_eq!(
        stat.dev,
        filesystem.stat("/").expect("stat root").dev,
        "files in one filesystem instance share its device id"
    );
    assert_eq!(stat.rdev, 0);

    let bytes = filesystem
        .read_file("/meta.txt")
        .expect("read truncated file");
    assert_eq!(&bytes[..5], b"hello");
    assert_eq!(&bytes[5..], &[0, 0, 0]);

    assert_eq!(
        filesystem
            .pread("/meta.txt", 2, 4)
            .expect("pread middle slice"),
        b"llo\0".to_vec()
    );
    assert!(filesystem
        .pread("/meta.txt", 100, 4)
        .expect("pread beyond eof")
        .is_empty());
}

#[test]
fn oversized_raw_truncate_and_pwrite_fail_without_mutating_file_contents() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/huge.txt", b"safe".to_vec())
        .expect("seed file");

    assert_error_code(filesystem.truncate("/huge.txt", u64::MAX), "ENOMEM");
    assert_eq!(
        filesystem
            .read_file("/huge.txt")
            .expect("read after failed truncate"),
        b"safe".to_vec()
    );

    assert_error_code(
        filesystem.pwrite("/huge.txt", b"x".to_vec(), u64::MAX),
        "ENOMEM",
    );
    assert_eq!(
        filesystem
            .read_file("/huge.txt")
            .expect("read after failed pwrite"),
        b"safe".to_vec()
    );
}

#[test]
fn sparse_pwrite_reports_only_allocated_blocks_and_survives_snapshots() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/sparse", Vec::new())
        .expect("create sparse file");
    filesystem
        .pwrite("/sparse", vec![b'x'; 50 * 1024], 1_600 * 1024)
        .expect("write sparse extent");

    let stat = filesystem.stat("/sparse").expect("stat sparse file");
    assert_eq!(stat.size, 1_650 * 1024);
    assert_eq!(stat.blocks, 100);
    assert!(stat.blocks * 512 < stat.size);

    let mut restored = MemoryFileSystem::from_snapshot(filesystem.snapshot());
    assert_eq!(
        restored
            .stat("/sparse")
            .expect("stat restored sparse file")
            .blocks,
        100
    );
    restored
        .truncate("/sparse", 1_610 * 1024)
        .expect("truncate sparse extent");
    assert_eq!(
        restored
            .stat("/sparse")
            .expect("stat truncated sparse file")
            .blocks,
        20
    );
}

#[test]
fn allocate_preserves_existing_bytes_and_accounts_for_the_requested_extent() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/allocated", b"prefix".to_vec())
        .expect("seed allocation target");

    filesystem
        .allocate("/allocated", 512, 1024)
        .expect("allocate range");

    let contents = filesystem
        .read_file("/allocated")
        .expect("read allocated file");
    assert_eq!(&contents[..6], b"prefix");
    assert!(contents[6..].iter().all(|byte| *byte == 0));
    let stat = filesystem.stat("/allocated").expect("stat allocated file");
    assert_eq!(stat.size, 1536);
    assert_eq!(stat.blocks, 3);
}

#[test]
fn punch_hole_zeroes_complete_blocks_without_changing_file_size() {
    let mut filesystem = MemoryFileSystem::new();
    let mut contents = vec![b'a'; 2048];
    contents[1536..].fill(b'z');
    filesystem
        .write_file("/punched", contents)
        .expect("seed punch target");

    filesystem
        .punch_hole("/punched", 512, 1024)
        .expect("punch aligned range");
    filesystem
        .punch_hole("/punched", 512, 1024)
        .expect("repeat punch is idempotent");

    let contents = filesystem.read_file("/punched").expect("read punch target");
    assert!(contents[..512].iter().all(|byte| *byte == b'a'));
    assert!(contents[512..1536].iter().all(|byte| *byte == 0));
    assert!(contents[1536..].iter().all(|byte| *byte == b'z'));
    let stat = filesystem.stat("/punched").expect("stat punch target");
    assert_eq!(stat.size, 2048);
    assert_eq!(stat.blocks, 2);
    assert_eq!(
        filesystem
            .allocated_ranges("/punched")
            .expect("map allocated ranges"),
        vec![(0, 512), (1536, 2048)]
    );
}

#[test]
fn zero_range_zeroes_exact_bytes_reallocates_holes_and_honors_keep_size() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/zeroed", vec![b'a'; 2048])
        .expect("seed zero-range target");
    filesystem
        .punch_hole("/zeroed", 512, 512)
        .expect("create source hole");

    filesystem
        .zero_range("/zeroed", 384, 768, false)
        .expect("zero unaligned range");
    let contents = filesystem.read_file("/zeroed").expect("read zeroed file");
    assert!(contents[..384].iter().all(|byte| *byte == b'a'));
    assert!(contents[384..1152].iter().all(|byte| *byte == 0));
    assert!(contents[1152..].iter().all(|byte| *byte == b'a'));
    assert_eq!(
        filesystem.allocated_ranges("/zeroed").unwrap(),
        vec![(0, 2048)]
    );
    assert_eq!(
        filesystem.unwritten_ranges("/zeroed").unwrap(),
        vec![(512, 1024)]
    );

    filesystem
        .zero_range("/zeroed", 3072, 512, true)
        .expect("zero beyond EOF with keep-size");
    assert_eq!(filesystem.stat("/zeroed").unwrap().size, 2048);
    filesystem
        .truncate("/zeroed", 3584)
        .expect("extend into reserved zero range");
    assert_eq!(
        filesystem.allocated_ranges("/zeroed").unwrap(),
        vec![(0, 2048), (3072, 3584)]
    );
    assert_eq!(
        filesystem.unwritten_ranges("/zeroed").unwrap(),
        vec![(512, 1024), (3072, 3584)]
    );
    filesystem
        .zero_range("/zeroed", 3584, 512, false)
        .expect("extend with zero range");
    assert_eq!(filesystem.stat("/zeroed").unwrap().size, 4096);
    assert_eq!(
        filesystem.allocated_ranges("/zeroed").unwrap(),
        vec![(0, 2048), (3072, 4096)]
    );
    assert_eq!(
        filesystem.unwritten_ranges("/zeroed").unwrap(),
        vec![(512, 1024), (3072, 4096)]
    );
    assert_eq!(
        (0..5)
            .map(|index| filesystem.extent_at("/zeroed", index).unwrap())
            .collect::<Vec<_>>(),
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
                end: 4096,
                unwritten: true,
            }),
            None,
        ]
    );
    filesystem
        .pwrite("/zeroed", vec![b'b'; 512], 512)
        .expect("convert one unwritten sector to data");
    assert_eq!(
        filesystem.unwritten_ranges("/zeroed").unwrap(),
        vec![(3072, 4096)]
    );
    assert_error_code(filesystem.zero_range("/zeroed", 0, 0, false), "EINVAL");
}

#[test]
fn insert_and_collapse_range_shift_bytes_and_sparse_extents() {
    let mut filesystem = MemoryFileSystem::new();
    let data = [
        vec![b'A'; 512],
        vec![b'B'; 512],
        vec![b'C'; 512],
        vec![b'D'; 512],
    ]
    .concat();
    filesystem.write_file("/shift", data).unwrap();
    filesystem.punch_hole("/shift", 512, 512).unwrap();

    filesystem.insert_range("/shift", 512, 512).unwrap();
    let inserted = filesystem.read_file("/shift").unwrap();
    assert_eq!(inserted.len(), 2560);
    assert_eq!(&inserted[..512], vec![b'A'; 512]);
    assert!(inserted[512..1536].iter().all(|byte| *byte == 0));
    assert_eq!(&inserted[1536..2048], vec![b'C'; 512]);
    assert_eq!(
        filesystem.allocated_ranges("/shift").unwrap(),
        vec![(0, 512), (1536, 2560)]
    );

    filesystem.collapse_range("/shift", 512, 512).unwrap();
    filesystem.collapse_range("/shift", 512, 512).unwrap();
    assert_eq!(
        filesystem.read_file("/shift").unwrap(),
        [vec![b'A'; 512], vec![b'C'; 512], vec![b'D'; 512]].concat()
    );
    assert_eq!(
        filesystem.allocated_ranges("/shift").unwrap(),
        vec![(0, 1536)]
    );
}

#[test]
fn directory_reads_and_metadata_updates_refresh_timestamps() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem
        .write_file("/workspace/file.txt", "hello")
        .expect("seed file");

    let before_dir_read = filesystem.stat("/workspace").expect("stat workspace");
    sleep(Duration::from_millis(2));
    filesystem
        .read_dir("/workspace")
        .expect("read workspace directory");
    let after_dir_read = filesystem.stat("/workspace").expect("restat workspace");
    assert!(
        after_dir_read.atime_ms > before_dir_read.atime_ms,
        "directory atime should advance after read_dir"
    );

    let before_link = filesystem.stat("/workspace/file.txt").expect("stat file");
    sleep(Duration::from_millis(2));
    filesystem
        .link("/workspace/file.txt", "/workspace/file-link.txt")
        .expect("create hard link");
    let after_link = filesystem.stat("/workspace/file.txt").expect("restat file");
    assert!(
        after_link.ctime_ms > before_link.ctime_ms,
        "ctime should advance when link count changes"
    );

    let before_rename = after_link.ctime_ms;
    sleep(Duration::from_millis(2));
    filesystem
        .rename("/workspace/file-link.txt", "/workspace/file-renamed.txt")
        .expect("rename linked path");
    let renamed = filesystem
        .stat("/workspace/file-renamed.txt")
        .expect("stat renamed path");
    assert!(
        renamed.ctime_ms > before_rename,
        "ctime should advance on rename"
    );
}

#[test]
fn read_dir_with_types_reports_direct_children() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/typed/file.txt", "f")
        .expect("write file child");
    filesystem
        .write_file("/typed/sub/nested.txt", "n")
        .expect("write nested child");
    filesystem
        .symlink("/typed/file.txt", "/typed/link.txt")
        .expect("write symlink child");

    let entries = filesystem
        .read_dir_with_types("/typed")
        .expect("read typed directory");

    let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, vec!["file.txt", "link.txt", "sub"]);

    let sub = entries
        .iter()
        .find(|entry| entry.name == "sub")
        .expect("sub directory should be present");
    assert!(sub.is_directory);
    assert!(!sub.is_symbolic_link);

    let link = entries
        .iter()
        .find(|entry| entry.name == "link.txt")
        .expect("symlink should be present");
    assert!(!link.is_directory);
    assert!(link.is_symbolic_link);
}

#[test]
fn memory_filesystem_snapshot_round_trips_hardlinks_and_symlinks() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/workspace/original.txt", "hello")
        .expect("write original");
    filesystem
        .link("/workspace/original.txt", "/workspace/linked.txt")
        .expect("create hard link");
    filesystem
        .symlink("/workspace/original.txt", "/workspace/alias.txt")
        .expect("create symlink");

    let snapshot = filesystem.snapshot();
    let mut restored = MemoryFileSystem::from_snapshot(snapshot);

    assert_eq!(
        restored
            .read_text_file("/workspace/linked.txt")
            .expect("read hard-linked file"),
        "hello"
    );
    assert_eq!(
        restored
            .read_text_file("/workspace/alias.txt")
            .expect("read symlink target"),
        "hello"
    );

    restored
        .write_file("/workspace/linked.txt", "updated")
        .expect("write through hard link");
    assert_eq!(
        restored
            .read_text_file("/workspace/original.txt")
            .expect("hard link should share inode"),
        "updated"
    );
    assert_eq!(
        restored
            .stat("/workspace/original.txt")
            .expect("stat restored hard link")
            .nlink,
        2
    );
}

#[test]
fn memory_filesystem_instances_have_distinct_device_ids() {
    let mut first = MemoryFileSystem::new();
    let mut second = MemoryFileSystem::new();
    first
        .write_file("/file.txt", "first")
        .expect("write file in first filesystem");
    second
        .write_file("/file.txt", "second")
        .expect("write file in second filesystem");

    let first_stat = first.stat("/file.txt").expect("stat first file");
    let second_stat = second.stat("/file.txt").expect("stat second file");

    // Inode numbers are only unique within one filesystem instance, so file
    // identity comparisons across layered or mounted compositions need
    // per-instance device ids.
    assert_eq!(first_stat.ino, second_stat.ino);
    assert_ne!(first_stat.dev, second_stat.dev);

    let restored = MemoryFileSystem::from_snapshot(first.snapshot());
    assert_ne!(
        restored.lstat("/file.txt").expect("stat restored file").dev,
        second_stat.dev
    );
}

#[test]
fn xattrs_follow_inode_identity_and_survive_snapshots() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem.write_file("/file", b"data").unwrap();
    filesystem.link("/file", "/hardlink").unwrap();

    filesystem
        .set_xattr("/file", "user.agentos", b"one".to_vec(), XATTR_CREATE, true)
        .unwrap();
    assert_eq!(
        filesystem
            .get_xattr("/hardlink", "user.agentos", true)
            .unwrap(),
        b"one"
    );
    assert_error_code(
        filesystem.set_xattr(
            "/file",
            "user.agentos",
            b"duplicate".to_vec(),
            XATTR_CREATE,
            true,
        ),
        "EEXIST",
    );
    filesystem
        .set_xattr(
            "/hardlink",
            "user.agentos",
            b"two".to_vec(),
            XATTR_REPLACE,
            true,
        )
        .unwrap();

    let mut restored = MemoryFileSystem::from_snapshot(filesystem.snapshot());
    assert_eq!(
        restored.get_xattr("/file", "user.agentos", true).unwrap(),
        b"two"
    );
    restored
        .remove_xattr("/file", "user.agentos", true)
        .unwrap();
    assert_error_code(
        restored.get_xattr("/hardlink", "user.agentos", true),
        "ENODATA",
    );
}

#[test]
fn xattr_value_and_name_list_limits_accept_boundary_and_rollback_plus_one() {
    let mut filesystem = MemoryFileSystem::new();
    filesystem.write_file("/value", b"data").unwrap();
    let exact_value = vec![b'x'; 64 * 1024];
    filesystem
        .set_xattr(
            "/value",
            "user.limit",
            exact_value.clone(),
            XATTR_CREATE,
            true,
        )
        .expect("64 KiB xattr value is Linux-valid");
    assert_error_code(
        filesystem.set_xattr(
            "/value",
            "user.limit",
            vec![b'y'; 64 * 1024 + 1],
            XATTR_REPLACE,
            true,
        ),
        "E2BIG",
    );
    assert_eq!(
        filesystem
            .get_xattr("/value", "user.limit", true)
            .expect("rejected replacement preserves old value"),
        exact_value
    );
    let oversized_name = format!("user.{}", "x".repeat(251));
    assert_eq!(oversized_name.len(), 256);
    assert_error_code(
        filesystem.set_xattr("/value", &oversized_name, Vec::new(), XATTR_CREATE, true),
        "EINVAL",
    );

    filesystem.write_file("/list", b"data").unwrap();
    for index in 0..256 {
        let name = format!("user.{index:04}.{}", "n".repeat(245));
        assert_eq!(name.len(), 255);
        filesystem
            .set_xattr("/list", &name, Vec::new(), XATTR_CREATE, true)
            .expect("name list entry within 64 KiB boundary");
    }
    let names = filesystem
        .list_xattrs("/list", true)
        .expect("exact 64 KiB xattr name list");
    assert_eq!(
        names.iter().map(|name| name.len() + 1).sum::<usize>(),
        64 * 1024
    );

    assert_error_code(
        filesystem.set_xattr("/list", "user.overflow", Vec::new(), XATTR_CREATE, true),
        "ENOSPC",
    );
    assert_eq!(
        filesystem.list_xattrs("/list", true).unwrap(),
        names,
        "name-list overflow must not partially insert the new attribute"
    );
}
