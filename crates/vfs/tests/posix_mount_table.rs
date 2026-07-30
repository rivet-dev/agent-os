use agentos_runtime_tokio::{RuntimeConfig, SidecarRuntime};
use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use vfs::adapter::MountedEngineFileSystem;
use vfs::engine::engines::{ChunkedFs, ChunkedFsOptions};
use vfs::engine::mem::{InMemoryMetadataStore, MemoryBlockStore};
use vfs::posix::{
    MemoryFileSystem, SingleSymlinkFileSystem, VfsResult, VirtualDirEntry, VirtualFileSystem,
    VirtualStat, VirtualUtimeSpec,
};
use vfs::posix::{MountOptions, MountTable, MountedFileSystem};

fn test_runtime_context() -> agentos_runtime_tokio::RuntimeContext {
    SidecarRuntime::process(&RuntimeConfig::default())
        .expect("create test runtime")
        .context()
}

struct ShutdownTrackingFileSystem {
    shutdown: Arc<AtomicBool>,
}

impl ShutdownTrackingFileSystem {
    fn new(shutdown: Arc<AtomicBool>) -> Self {
        Self { shutdown }
    }
}

impl MountedFileSystem for ShutdownTrackingFileSystem {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        unreachable!("failed mount should not read {path}")
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        unreachable!("failed mount should not read dir {path}")
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        unreachable!("failed mount should not read dir types {path}")
    }

    fn write_file(&mut self, path: &str, _content: Vec<u8>) -> VfsResult<()> {
        unreachable!("failed mount should not write {path}")
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not create dir {path}")
    }

    fn mkdir(&mut self, path: &str, _recursive: bool) -> VfsResult<()> {
        unreachable!("failed mount should not mkdir {path}")
    }

    fn exists(&self, _path: &str) -> bool {
        false
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        unreachable!("failed mount should not stat {path}")
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not remove file {path}")
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not remove dir {path}")
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not rename {old_path} to {new_path}")
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        unreachable!("failed mount should not realpath {path}")
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not symlink {target} to {link_path}")
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        unreachable!("failed mount should not readlink {path}")
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        unreachable!("failed mount should not lstat {path}")
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        unreachable!("failed mount should not link {old_path} to {new_path}")
    }

    fn chmod(&mut self, path: &str, _mode: u32) -> VfsResult<()> {
        unreachable!("failed mount should not chmod {path}")
    }

    fn chown(&mut self, path: &str, _uid: u32, _gid: u32) -> VfsResult<()> {
        unreachable!("failed mount should not chown {path}")
    }

    fn utimes(&mut self, path: &str, _atime_ms: u64, _mtime_ms: u64) -> VfsResult<()> {
        unreachable!("failed mount should not utimes {path}")
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        _atime: VirtualUtimeSpec,
        _mtime: VirtualUtimeSpec,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        unreachable!("failed mount should not utimes_spec {path}")
    }

    fn truncate(&mut self, path: &str, _length: u64) -> VfsResult<()> {
        unreachable!("failed mount should not truncate {path}")
    }

    fn insert_range(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        unreachable!("failed mount should not insert range in {path}")
    }

    fn collapse_range(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        unreachable!("failed mount should not collapse range in {path}")
    }

    fn pread(&mut self, path: &str, _offset: u64, _length: usize) -> VfsResult<Vec<u8>> {
        unreachable!("failed mount should not pread {path}")
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        self.shutdown.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn mount_table_prefers_mounted_filesystems_and_merges_mount_points() {
    let mut root = MemoryFileSystem::new();
    root.write_file("/data/root-only.txt", b"root".to_vec())
        .expect("seed root file");

    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/mounted.txt", b"mounted".to_vec())
        .expect("seed mounted file");

    let mut table = MountTable::new(root);
    table
        .mount("/data", mounted, MountOptions::new("memory"))
        .expect("mount memory filesystem");

    assert_eq!(
        table
            .read_file("/data/mounted.txt")
            .expect("read mounted file"),
        b"mounted".to_vec()
    );
    assert!(!table.exists("/data/root-only.txt"));

    let root_entries = table.read_dir("/").expect("read root directory");
    assert!(root_entries.contains(&String::from("data")));
}

#[test]
fn mount_table_tracks_plugin_guest_source_and_filesystem_type_separately() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount(
            "/data",
            MemoryFileSystem::new(),
            MountOptions::new("chunked_local")
                .guest_source("/dev/agentos-test")
                .guest_fstype("agentos"),
        )
        .expect("mount agentos filesystem");

    let entry = table
        .get_mounts()
        .into_iter()
        .find(|entry| entry.path == "/data")
        .expect("mounted entry");
    assert_eq!(entry.plugin_id, "chunked_local");
    assert_eq!(entry.guest_source, "/dev/agentos-test");
    assert_eq!(entry.guest_fstype, "agentos");
}

#[test]
fn mount_table_enforces_read_only_and_cross_mount_boundaries() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount(
            "/readonly",
            MemoryFileSystem::new(),
            MountOptions::new("memory").read_only(true),
        )
        .expect("mount readonly filesystem");
    table
        .mount(
            "/writable",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .expect("mount writable filesystem");

    let read_only_error = table
        .write_file("/readonly/blocked.txt", b"blocked".to_vec())
        .expect_err("readonly mount should reject writes");
    assert_eq!(read_only_error.code(), "EROFS");

    table
        .write_file("/writable/file.txt", b"ok".to_vec())
        .expect("write mounted file");
    let cross_mount_error = table
        .rename("/writable/file.txt", "/file.txt")
        .expect_err("rename across mounts should fail");
    assert_eq!(cross_mount_error.code(), "EXDEV");
}

#[test]
fn mount_table_allows_symlink_targets_across_mount_boundaries() {
    let mut root = MemoryFileSystem::new();
    root.write_file("/root.txt", b"root".to_vec())
        .expect("seed root file");

    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/inside.txt", b"inside".to_vec())
        .expect("seed mounted file");

    let mut table = MountTable::new(root);
    table
        .mount("/mounted", mounted, MountOptions::new("memory"))
        .expect("mount memory filesystem");

    table
        .symlink("../root.txt", "/mounted/root-link")
        .expect("symlink targets are path strings and may cross mounts");
    assert_eq!(
        table.read_link("/mounted/root-link").unwrap(),
        "../root.txt"
    );
    assert_eq!(
        table.read_file("/mounted/root-link").unwrap(),
        b"root".to_vec()
    );
}

#[test]
fn mount_table_rejects_hardlinks_that_cross_mount_boundaries() {
    let mut root = MemoryFileSystem::new();
    root.write_file("/root.txt", b"root".to_vec())
        .expect("seed root file");

    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/inside.txt", b"inside".to_vec())
        .expect("seed mounted file");

    let mut table = MountTable::new(root);
    table
        .mount("/mounted", mounted, MountOptions::new("memory"))
        .expect("mount memory filesystem");

    let error = table
        .link("/root.txt", "/mounted/root-link")
        .expect_err("cross-mount hardlink should fail");
    assert_eq!(error.code(), "EXDEV");
}

#[test]
fn mount_table_realpath_follows_symlinks_across_leaf_mounts() {
    let mut table = MountTable::new(MemoryFileSystem::new());

    table
        .mount_boxed(
            "/opt/agentos/bin/pi",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("../pkgs/pi/current/bin/pi"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount command symlink leaf");

    table
        .mount_boxed(
            "/opt/agentos/pkgs/pi/current",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("1.2.3"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount current symlink leaf");

    let mut content = MemoryFileSystem::new();
    content
        .mkdir("/bin", true)
        .expect("seed package bin directory");
    content
        .write_file("/bin/pi", b"#!/bin/sh\n".to_vec())
        .expect("seed package command");
    table
        .mount(
            "/opt/agentos/pkgs/pi/1.2.3",
            content,
            MountOptions::new("tar").read_only(true),
        )
        .expect("mount package content leaf");

    assert_eq!(
        table
            .realpath("/opt/agentos/bin/pi")
            .expect("realpath across command/current/content mounts"),
        "/opt/agentos/pkgs/pi/1.2.3/bin/pi"
    );
}

#[test]
fn mount_table_realpath_rebases_mounted_absolute_link_after_leaf_mounts() {
    let mut table = MountTable::new(MemoryFileSystem::new());

    table
        .mount_boxed(
            "/opt/agentos/bin/pi",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("../pkgs/pi/current/bin/pi"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount command symlink leaf");
    table
        .mount_boxed(
            "/opt/agentos/pkgs/pi/current",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("1.2.3"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount current symlink leaf");

    let mut content = MemoryFileSystem::new();
    content
        .mkdir("/bin", true)
        .expect("seed package bin directory");
    content
        .write_file("/adapter.mjs", b"adapter".to_vec())
        .expect("seed package adapter");
    content
        .symlink("/adapter.mjs", "/bin/pi")
        .expect("seed mount-local command symlink");
    table
        .mount(
            "/opt/agentos/pkgs/pi/1.2.3",
            content,
            MountOptions::new("package")
                .read_only(true)
                .absolute_symlinks_mount_relative(true),
        )
        .expect("mount package content leaf");

    assert_eq!(
        table
            .realpath("/opt/agentos/bin/pi")
            .expect("resolve command through leaf and package symlinks"),
        "/opt/agentos/pkgs/pi/1.2.3/adapter.mjs"
    );
}

#[test]
fn mount_table_realpath_keeps_mount_local_absolute_symlinks_inside_mount() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/target.txt", b"target".to_vec())
        .expect("seed mount target");
    mounted
        .symlink("/target.txt", "/link.txt")
        .expect("seed mount-local absolute symlink");

    table
        .mount(
            "/mnt",
            mounted,
            MountOptions::new("memory").absolute_symlinks_mount_relative(true),
        )
        .expect("mount memory filesystem");

    assert_eq!(
        table
            .realpath("/mnt/link.txt")
            .expect("realpath through mount-local absolute symlink"),
        "/mnt/target.txt"
    );
    assert_eq!(
        table
            .read_link("/mnt/link.txt")
            .expect("read mount-local absolute symlink"),
        "/mnt/target.txt"
    );
}

#[test]
fn mount_table_content_ops_follow_guest_absolute_symlink_targets() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    let mut mounted = MemoryFileSystem::new();
    mounted.mkdir("/target/child", true).unwrap();
    mounted
        .write_file("/target/child/file", b"content".to_vec())
        .unwrap();
    table
        .mount("/mnt", mounted, MountOptions::new("memory"))
        .unwrap();
    table.symlink("/mnt/target", "/mnt/link").unwrap();

    assert_eq!(table.read_link("/mnt/link").unwrap(), "/mnt/target");
    assert!(table.stat("/mnt/link/child").unwrap().is_directory);
    assert_eq!(table.read_file("/mnt/link/child/file").unwrap(), b"content");
    table
        .set_xattr(
            "/mnt/target/child/file",
            "trusted.note",
            b"value".to_vec(),
            0,
            false,
        )
        .unwrap();
    assert_eq!(
        table
            .get_xattr("/mnt/link/child/file", "trusted.note", false)
            .unwrap(),
        b"value"
    );
}

#[test]
fn mount_table_guest_absolute_symlinks_cross_mount_boundaries() {
    let mut root = MemoryFileSystem::new();
    root.mkdir("/test", true).unwrap();
    root.write_file("/test/target", b"target".to_vec()).unwrap();

    let mut table = MountTable::new(root);
    table
        .mount(
            "/scratch",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .unwrap();
    table.symlink("/test/target", "/scratch/link").unwrap();

    assert_eq!(table.read_link("/scratch/link").unwrap(), "/test/target");
    assert_eq!(table.realpath("/scratch/link").unwrap(), "/test/target");
    assert_eq!(table.read_file("/scratch/link").unwrap(), b"target");
    table.remount("/scratch", "remount,ro").unwrap();
    table.truncate("/scratch/link", 0).unwrap();
    assert!(table.read_file("/test/target").unwrap().is_empty());
}

#[test]
fn mount_table_lchown_updates_the_symlink_not_its_target() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    let mut mounted = MemoryFileSystem::new();
    mounted
        .write_file("/target.txt", b"target".to_vec())
        .expect("seed mount target");
    mounted
        .chown("/target.txt", 10, 20)
        .expect("seed target ownership");
    mounted
        .symlink("/target.txt", "/link.txt")
        .expect("seed mount-local absolute symlink");
    table
        .mount("/mnt", mounted, MountOptions::new("memory"))
        .expect("mount memory filesystem");

    table
        .lchown("/mnt/link.txt", 30, 40)
        .expect("lchown mounted symlink");

    let link = table.lstat("/mnt/link.txt").expect("lstat symlink");
    assert_eq!((link.uid, link.gid), (30, 40));
    let target = table.stat("/mnt/target.txt").expect("stat target");
    assert_eq!((target.uid, target.gid), (10, 20));
}

#[test]
fn leaf_mounts_coexist_with_user_files_in_writable_parent_directory() {
    let mut table = MountTable::new(MemoryFileSystem::new());

    table
        .mount_boxed(
            "/opt/agentos/bin/pi",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("../pkgs/pi/current/bin/pi"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount managed command leaf");

    let mut content = MemoryFileSystem::new();
    content
        .mkdir("/bin", true)
        .expect("seed package bin directory");
    content
        .write_file("/bin/pi", b"#!/bin/sh\n".to_vec())
        .expect("seed executable package command");
    content
        .chmod("/bin/pi", 0o755)
        .expect("chmod package command executable");
    table
        .mount(
            "/opt/agentos/pkgs/pi/1.2.3",
            content,
            MountOptions::new("tar").read_only(true),
        )
        .expect("mount package content leaf");
    table
        .mount_boxed(
            "/opt/agentos/pkgs/pi/current",
            Box::new(vfs::posix::MountedVirtualFileSystem::new(
                SingleSymlinkFileSystem::new("1.2.3"),
            )),
            MountOptions::new("single-symlink").read_only(true),
        )
        .expect("mount current symlink leaf");

    table
        .write_file("/opt/agentos/bin/user-tool", b"#!/bin/sh\n".to_vec())
        .expect("user install writes into writable parent dir");
    table
        .chmod("/opt/agentos/bin/user-tool", 0o755)
        .expect("chmod user command executable");

    let entries = table
        .read_dir("/opt/agentos/bin")
        .expect("list merged managed/user bin dir");
    assert!(entries.contains(&String::from("pi")));
    assert!(entries.contains(&String::from("user-tool")));

    let managed_realpath = table
        .realpath("/opt/agentos/bin/pi")
        .expect("managed command realpath");
    assert_eq!(managed_realpath, "/opt/agentos/pkgs/pi/1.2.3/bin/pi");
    assert_eq!(
        table
            .stat(&managed_realpath)
            .expect("managed command stat")
            .mode
            & 0o111,
        0o111
    );
    assert_eq!(
        table
            .stat("/opt/agentos/bin/user-tool")
            .expect("user command stat")
            .mode
            & 0o111,
        0o111
    );
}

#[test]
fn mount_table_mounts_nested_filesystems_under_read_only_parents() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount(
            "/root/node_modules",
            MemoryFileSystem::new(),
            MountOptions::new("memory").read_only(true),
        )
        .expect("mount read-only parent filesystem");

    let mut nested = MemoryFileSystem::new();
    nested
        .write_file("/package.json", b"{}".to_vec())
        .expect("seed nested package file");

    table
        .mount(
            "/root/node_modules/@scope/pkg",
            nested,
            MountOptions::new("memory").read_only(true),
        )
        .expect("read-only parents must still accept nested mounts");

    assert_eq!(
        table
            .read_file("/root/node_modules/@scope/pkg/package.json")
            .expect("read file through nested mount"),
        b"{}".to_vec()
    );
}

#[test]
fn mount_table_rejects_mount_when_mount_point_creation_fails() {
    let mut root = MemoryFileSystem::new();
    root.write_file("/blocked", b"not a directory".to_vec())
        .expect("seed file at parent path");
    let mut table = MountTable::new(root);

    let error = table
        .mount(
            "/blocked/child",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .expect_err("mount point creation should fail through file parent");

    assert_eq!(error.code(), "ENOTDIR");
    assert!(!table
        .get_mounts()
        .iter()
        .any(|mount| mount.path == "/blocked/child"));
}

#[test]
fn mount_table_shuts_down_boxed_filesystem_when_mount_point_creation_fails() {
    let mut root = MemoryFileSystem::new();
    root.write_file("/blocked", b"not a directory".to_vec())
        .expect("seed file at parent path");
    let mut table = MountTable::new(root);
    let shutdown = Arc::new(AtomicBool::new(false));

    let error = table
        .mount_boxed(
            "/blocked/child",
            Box::new(ShutdownTrackingFileSystem::new(Arc::clone(&shutdown))),
            MountOptions::new("tracking"),
        )
        .expect_err("mount point creation should fail through file parent");

    assert_eq!(error.code(), "ENOTDIR");
    assert!(shutdown.load(Ordering::SeqCst));
}

#[test]
fn mount_table_unmount_rejects_parent_mounts_with_children() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount("/a", MemoryFileSystem::new(), MountOptions::new("parent"))
        .expect("mount parent filesystem");
    table
        .mount("/a/b", MemoryFileSystem::new(), MountOptions::new("child"))
        .expect("mount child filesystem");

    let error = table
        .unmount("/a")
        .expect_err("parent mount should stay busy while child mount exists");
    assert_eq!(error.code(), "EBUSY");
}

#[test]
fn mount_table_unmount_succeeds_after_children_are_removed() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount("/a", MemoryFileSystem::new(), MountOptions::new("parent"))
        .expect("mount parent filesystem");
    table
        .mount("/a/b", MemoryFileSystem::new(), MountOptions::new("child"))
        .expect("mount child filesystem");

    table.unmount("/a/b").expect("unmount child first");
    table.unmount("/a").expect("unmount parent after child");
}

#[test]
fn mount_table_does_not_alias_paths_that_repeat_the_mount_segment() {
    // Regression: `resolve_index` previously stripped the mount prefix with
    // `trim_start_matches`, which removes *every* leading repetition. For a
    // mount `/data`, `/data/database.sqlite` was mangled to `/base.sqlite`
    // (because `/database.sqlite` still starts with `/data`), so a read of one
    // file silently returned a different file within the mount.
    let mut backing = MemoryFileSystem::new();
    backing
        .write_file("/database.sqlite", b"REAL".to_vec())
        .expect("seed real file");
    backing
        .write_file("/base.sqlite", b"DECOY".to_vec())
        .expect("seed decoy file");

    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount("/data", backing, MountOptions::new("memory"))
        .expect("mount memory filesystem");

    assert_eq!(
        table.read_file("/data/database.sqlite").expect("read file"),
        b"REAL".to_vec(),
        "path must map to the file the caller named, not an aliased one"
    );

    // A genuinely nested directory named like the mount must also resolve right.
    let mut nested = MemoryFileSystem::new();
    nested.mkdir("/data", true).expect("nested dir");
    nested
        .write_file("/data/file.txt", b"NESTED".to_vec())
        .expect("seed nested file");
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount("/data", nested, MountOptions::new("memory"))
        .expect("mount nested filesystem");
    assert_eq!(
        table.read_file("/data/data/file.txt").expect("read nested"),
        b"NESTED".to_vec()
    );
}

#[test]
fn remount_enforces_atime_and_read_only_policies_without_changing_other_times() {
    let engine = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions::default(),
    );
    let mut mounted = MountedEngineFileSystem::with_runtime_context(engine, test_runtime_context());
    mounted
        .mkdir("/dir", true)
        .expect("create mounted directory");
    mounted
        .write_file("/dir/file", b"data".to_vec())
        .expect("create mounted file");

    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount_boxed("/data", Box::new(mounted), MountOptions::new("memory"))
        .expect("mount chunked filesystem");

    let initial = table.stat("/data/dir/file").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert_eq!(table.read_file("/data/dir/file").unwrap(), b"data");
    let first_read = table.stat("/data/dir/file").unwrap();
    assert!(first_read.atime_ms > initial.atime_ms);
    assert_eq!(first_read.mtime_ms, initial.mtime_ms);
    assert_eq!(first_read.ctime_ms, initial.ctime_ms);

    std::thread::sleep(Duration::from_millis(5));
    table.read_file("/data/dir/file").unwrap();
    assert_eq!(
        table.stat("/data/dir/file").unwrap().atime_ms,
        first_read.atime_ms,
        "relatime must not update a second read after atime is newer than mtime and ctime"
    );

    table.remount("/data", "remount,strictatime").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    table.read_file("/data/dir/file").unwrap();
    let strict_read = table.stat("/data/dir/file").unwrap();
    assert!(strict_read.atime_ms > first_read.atime_ms);

    table.remount("/data", "remount,noatime").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    table.read_file("/data/dir/file").unwrap();
    assert_eq!(
        table.stat("/data/dir/file").unwrap().atime_ms,
        strict_read.atime_ms
    );

    table
        .remount("/data", "remount,relatime,nodiratime,nosuid")
        .unwrap();
    let directory_before = table.stat("/data/dir").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    table.read_dir("/data/dir").unwrap();
    assert_eq!(
        table.stat("/data/dir").unwrap().atime_ms,
        directory_before.atime_ms
    );

    table.remount("/data", "remount,ro,strictatime").unwrap();
    let read_only_before = table.stat("/data/dir/file").unwrap();
    std::thread::sleep(Duration::from_millis(5));
    table.read_file("/data/dir/file").unwrap();
    assert_eq!(
        table.stat("/data/dir/file").unwrap().atime_ms,
        read_only_before.atime_ms
    );
    assert_eq!(
        table
            .write_file("/data/dir/file", b"blocked".to_vec())
            .unwrap_err()
            .code(),
        "EROFS"
    );
    assert_eq!(
        table
            .get_mounts()
            .into_iter()
            .find(|mount| mount.path == "/data")
            .unwrap()
            .option_string(),
        "ro,strictatime,nodiratime,nosuid"
    );

    table.remount("/data", "remount,suid").unwrap();
    assert_eq!(
        table
            .get_mounts()
            .into_iter()
            .find(|mount| mount.path == "/data")
            .unwrap()
            .option_string(),
        "ro,strictatime,nodiratime"
    );
}

#[test]
fn mounted_chunked_unlink_does_not_follow_a_self_referential_symlink() {
    let engine = ChunkedFs::with_options(
        InMemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedFsOptions::default(),
    );
    let mounted = MountedEngineFileSystem::with_runtime_context(engine, test_runtime_context());
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount_boxed("/data", Box::new(mounted), MountOptions::new("memory"))
        .expect("mount chunked filesystem");
    table
        .symlink("self", "/data/self")
        .expect("create self-referential symlink");

    table
        .remove_file("/data/self")
        .expect("unlink the symlink entry without following it");

    assert_eq!(table.lstat("/data/self").unwrap_err().code(), "ENOENT");
}

#[test]
fn remount_rejects_unknown_options_and_non_mount_paths() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount(
            "/data",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .unwrap();
    assert_eq!(
        table
            .remount("/data", "remount,ro,lazytime")
            .unwrap_err()
            .code(),
        "EINVAL"
    );
    assert_eq!(
        table
            .get_mounts()
            .into_iter()
            .find(|mount| mount.path == "/data")
            .unwrap()
            .option_string(),
        "rw,relatime",
        "an invalid remount must not partially apply earlier options"
    );
    assert_eq!(
        table
            .remount("/data/not-a-mount", "remount,ro")
            .unwrap_err()
            .code(),
        "EINVAL"
    );
}

#[test]
fn mounted_capacity_reports_usage_enforces_enospc_and_reclaims_space() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    table
        .mount(
            "/data",
            MemoryFileSystem::new(),
            MountOptions::new("memory")
                .max_bytes(Some(8))
                .max_inodes(Some(4)),
        )
        .unwrap();

    let empty = table.path_stats("/data", None, None).unwrap();
    assert_eq!(empty.total_bytes, 8);
    assert_eq!(empty.used_bytes, 0);
    assert_eq!(empty.available_bytes, 8);

    table.write_file("/data/file", b"1234".to_vec()).unwrap();
    let written = table.path_stats("/data/file", None, None).unwrap();
    assert_eq!(written.used_bytes, 4);
    assert_eq!(written.available_bytes, 4);

    let error = table
        .pwrite("/data/file", b"56789".to_vec(), 4)
        .unwrap_err();
    assert_eq!(error.code(), "ENOSPC");
    assert_eq!(table.read_file("/data/file").unwrap(), b"1234");

    assert_eq!(
        table.remount("/data", "remount,size=3").unwrap_err().code(),
        "ENOSPC"
    );
    assert_eq!(
        table.path_stats("/data", None, None).unwrap().total_bytes,
        8
    );

    table.remount("/data", "remount,size=6").unwrap();
    table.pwrite("/data/file", b"56".to_vec(), 4).unwrap();
    assert_eq!(
        table
            .path_stats("/data", None, None)
            .unwrap()
            .available_bytes,
        0
    );
    table.remove_file("/data/file").unwrap();
    table
        .write_file("/data/reused", b"123456".to_vec())
        .expect("cached capacity must be reclaimed without a statfs rescan");
    table.remove_file("/data/reused").unwrap();

    table.mkdir("/data/a/b/c", true).unwrap();
    assert_eq!(table.create_dir("/data/d").unwrap_err().code(), "ENOSPC");
    table.remove_dir("/data/a/b/c").unwrap();
    table.remove_dir("/data/a/b").unwrap();
    table.remove_dir("/data/a").unwrap();
    table
        .mkdir("/data/d/e/f", true)
        .expect("removed directory inodes must be reclaimed without a statfs rescan");
    table.remove_dir("/data/d/e/f").unwrap();
    table.remove_dir("/data/d/e").unwrap();
    table.remove_dir("/data/d").unwrap();

    let reclaimed = table.path_stats("/data", None, None).unwrap();
    assert_eq!(reclaimed.used_bytes, 0);
    assert_eq!(reclaimed.available_bytes, 6);
}

#[test]
fn mutation_generation_tracks_successful_changes_only() {
    let mut table = MountTable::new(MemoryFileSystem::new());
    let initial = table.mutation_generation();

    assert!(!table.exists("/missing"));
    assert_eq!(
        table.mutation_generation(),
        initial,
        "read-only probes must not invalidate derived caches"
    );

    table
        .write_file("/module.js", b"module.exports = 1".to_vec())
        .unwrap();
    let after_write = table.mutation_generation();
    assert_ne!(after_write, initial);

    table.read_file("/module.js").unwrap();
    assert_eq!(
        table.mutation_generation(),
        after_write,
        "content reads must not invalidate derived caches"
    );

    table.rename("/module.js", "/renamed.js").unwrap();
    let after_rename = table.mutation_generation();
    assert_ne!(after_rename, after_write);

    assert_eq!(table.remove_file("/missing").unwrap_err().code(), "ENOENT");
    assert_eq!(
        table.mutation_generation(),
        after_rename,
        "rejected mutations must not invalidate derived caches"
    );

    table
        .mount(
            "/packages",
            MemoryFileSystem::new(),
            MountOptions::new("memory"),
        )
        .unwrap();
    assert_ne!(table.mutation_generation(), after_rename);
}
