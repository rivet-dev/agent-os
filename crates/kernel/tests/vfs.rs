use agent_os_kernel::vfs::{normalize_path, MemoryFileSystem, VirtualFileSystem, S_IFLNK, S_IFREG};
use std::{fmt::Debug, thread::sleep, time::Duration};

fn assert_error_code<T: Debug>(result: agent_os_kernel::vfs::VfsResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
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
fn path_traversal_stays_within_the_virtual_root() {
    let mut filesystem = MemoryFileSystem::new();

    filesystem
        .write_file("/etc/passwd", "guest-root")
        .expect("write virtual passwd");

    assert_eq!(
        filesystem
            .read_text_file("../../../etc/passwd")
            .expect("read virtual traversal target"),
        "guest-root"
    );
    assert_eq!(
        filesystem
            .realpath("../../../etc/passwd")
            .expect("realpath virtual traversal target"),
        "/etc/passwd"
    );
}

#[test]
fn paths_with_nul_bytes_are_rejected() {
    let mut filesystem = MemoryFileSystem::new();
    let nul_path = "/tmp/evil\0suffix";

    assert_error_code(filesystem.write_file(nul_path, "blocked"), "EINVAL");
    assert_error_code(filesystem.stat(nul_path), "EINVAL");
    assert_error_code(filesystem.mkdir(nul_path, true), "EINVAL");
}

#[test]
fn overlong_path_components_are_rejected() {
    let mut filesystem = MemoryFileSystem::new();
    let component = "a".repeat(256);
    let long_path = format!("/{component}/file.txt");

    assert_error_code(filesystem.write_file(&long_path, "blocked"), "ENAMETOOLONG");
    assert_error_code(filesystem.stat(&long_path), "ENAMETOOLONG");
}

#[test]
fn symlink_chains_beyond_linux_limit_return_eloop() {
    let mut filesystem = MemoryFileSystem::new();
    let symlink_limit = 40;

    filesystem
        .write_file("/target.txt", "reachable")
        .expect("write target");
    for index in 0..=symlink_limit {
        let source = format!("/chain-{index}.txt");
        let target = if index == symlink_limit {
            String::from("/target.txt")
        } else {
            format!("/chain-{}.txt", index + 1)
        };
        filesystem
            .symlink(&target, &source)
            .expect("create chain entry");
    }

    assert_error_code(filesystem.read_file("/chain-0.txt"), "ELOOP");
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
    assert_eq!(stat.dev, 1);
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
