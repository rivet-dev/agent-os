use std::collections::BTreeMap;
use vfs::posix::{
    decode_snapshot, decode_snapshot_with_import_limits, encode_snapshot, FilesystemEntry,
    MemoryFileSystemSnapshot, MemoryFileSystemSnapshotInode, MemoryFileSystemSnapshotInodeKind,
    MemoryFileSystemSnapshotMetadata, RootFileSystem, RootFilesystemDescriptor,
    RootFilesystemImportLimits, RootFilesystemMode, RootFilesystemResourceLimits,
    RootFilesystemSnapshot, ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use vfs::posix::{MemoryFileSystem, VirtualFileSystem, S_IFDIR, S_IFLNK, S_IFREG};
use vfs::posix::{OverlayFileSystem, OverlayMode};

#[derive(Debug, Clone, Copy, Default)]
struct TestResourceLimits {
    max_filesystem_bytes: Option<u64>,
    max_inode_count: Option<usize>,
}

impl RootFilesystemResourceLimits for TestResourceLimits {
    fn max_filesystem_bytes(&self) -> Option<u64> {
        self.max_filesystem_bytes
    }

    fn max_inode_count(&self) -> Option<usize> {
        self.max_inode_count
    }
}

fn directory_metadata(ino: u64) -> MemoryFileSystemSnapshotMetadata {
    MemoryFileSystemSnapshotMetadata {
        mode: S_IFDIR | 0o755,
        uid: 1000,
        gid: 1000,
        nlink: 2,
        ino,
        atime_ms: 0,
        atime_nsec: 0,
        mtime_ms: 0,
        mtime_nsec: 0,
        ctime_ms: 0,
        ctime_nsec: 0,
        birthtime_ms: 0,
        allocated_extents: Vec::new(),
        unwritten_extents: Vec::new(),
        xattrs: BTreeMap::new(),
    }
}

fn directory_inode(ino: u64) -> MemoryFileSystemSnapshotInode {
    MemoryFileSystemSnapshotInode {
        metadata: directory_metadata(ino),
        kind: MemoryFileSystemSnapshotInodeKind::Directory,
    }
}

fn deep_directory_tree(child_depth: usize) -> MemoryFileSystem {
    let mut path_index = BTreeMap::new();
    let mut inodes = BTreeMap::new();
    let mut next_ino = 1;

    path_index.insert(String::from("/"), next_ino);
    inodes.insert(next_ino, directory_inode(next_ino));
    next_ino += 1;

    let mut path = String::from("/deep");
    path_index.insert(path.clone(), next_ino);
    inodes.insert(next_ino, directory_inode(next_ino));
    next_ino += 1;

    for _ in 0..child_depth {
        path.push_str("/d");
        path_index.insert(path.clone(), next_ino);
        inodes.insert(next_ino, directory_inode(next_ino));
        next_ino += 1;
    }

    MemoryFileSystem::from_snapshot(MemoryFileSystemSnapshot {
        path_index,
        inodes,
        next_ino,
    })
}

fn assert_error_code<T: std::fmt::Debug>(result: Result<T, vfs::posix::VfsError>, expected: &str) {
    let error = result.expect_err("expected operation to fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn overlay_filesystem_prefers_higher_lowers_and_hides_whiteouts() {
    let mut higher = MemoryFileSystem::new();
    let mut lower = MemoryFileSystem::new();

    higher.mkdir("/etc", true).expect("create higher /etc");
    lower.mkdir("/etc", true).expect("create lower /etc");
    higher
        .write_file("/etc/config.txt", b"higher".to_vec())
        .expect("seed higher file");
    lower
        .write_file("/etc/config.txt", b"lower".to_vec())
        .expect("seed lower file");
    lower
        .write_file("/etc/only-lower.txt", b"lower-only".to_vec())
        .expect("seed lower-only file");

    let mut overlay = OverlayFileSystem::new(vec![higher, lower], OverlayMode::Ephemeral);
    assert_eq!(
        overlay
            .read_file("/etc/config.txt")
            .expect("read merged config"),
        b"higher".to_vec()
    );
    assert_eq!(
        overlay
            .read_file("/etc/only-lower.txt")
            .expect("read lower-only file"),
        b"lower-only".to_vec()
    );

    overlay
        .remove_file("/etc/only-lower.txt")
        .expect("whiteout lower file");
    assert!(!overlay.exists("/etc/only-lower.txt"));

    let entries = overlay.read_dir("/etc").expect("read merged directory");
    assert_eq!(entries, vec![String::from("config.txt")]);
}

#[test]
fn overlay_root_stat_uses_highest_lower_metadata() {
    let mut higher = MemoryFileSystem::new();
    let mut lower = MemoryFileSystem::new();

    higher.chown("/", 0, 0).expect("set higher root owner");
    lower.chown("/", 2000, 3000).expect("set lower root owner");

    let overlay = OverlayFileSystem::new(vec![higher, lower], OverlayMode::Ephemeral);
    let stat = overlay.lstat("/").expect("lstat merged root");

    assert_eq!(stat.uid, 0);
    assert_eq!(stat.gid, 0);
}

#[test]
fn overlay_rename_moves_lower_directory_trees_without_losing_children() {
    let mut lower = MemoryFileSystem::new();
    lower
        .mkdir("/src/nested", true)
        .expect("create lower directory tree");
    lower
        .write_file("/src/nested/child.txt", b"nested".to_vec())
        .expect("seed nested child");
    lower
        .write_file("/src/root.txt", b"root".to_vec())
        .expect("seed root child");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .rename("/src", "/dst")
        .expect("rename lower directory");

    assert_eq!(
        overlay
            .read_file("/dst/nested/child.txt")
            .expect("read renamed nested child"),
        b"nested".to_vec()
    );
    assert_eq!(
        overlay
            .read_file("/dst/root.txt")
            .expect("read renamed root child"),
        b"root".to_vec()
    );
    assert_error_code(overlay.read_file("/src/nested/child.txt"), "ENOENT");
}

#[test]
fn overlay_rename_preserves_symlinks_instead_of_dereferencing_them() {
    let mut lower = MemoryFileSystem::new();
    lower
        .write_file("/target.txt", b"target".to_vec())
        .expect("seed symlink target");
    lower
        .symlink("/target.txt", "/alias.txt")
        .expect("create lower symlink");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .rename("/alias.txt", "/alias-renamed.txt")
        .expect("rename symlink");

    assert!(
        overlay
            .lstat("/alias-renamed.txt")
            .expect("lstat renamed symlink")
            .is_symbolic_link
    );
    assert_eq!(
        overlay
            .read_link("/alias-renamed.txt")
            .expect("read renamed symlink target"),
        String::from("/target.txt")
    );
    assert_error_code(overlay.read_link("/alias.txt"), "ENOENT");
}

#[test]
fn overlay_remove_dir_rejects_lower_only_children_in_merged_view() {
    let mut lower = MemoryFileSystem::new();
    lower
        .mkdir("/tmp/nonempty", true)
        .expect("create lower directory");
    lower
        .write_file("/tmp/nonempty/child.txt", b"child".to_vec())
        .expect("seed lower child");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    assert_error_code(overlay.remove_dir("/tmp/nonempty"), "ENOTEMPTY");
    assert!(overlay.exists("/tmp/nonempty/child.txt"));
}

#[test]
fn overlay_remove_dir_rejects_lower_children_after_directory_copy_up() {
    let mut lower = MemoryFileSystem::new();
    lower
        .mkdir("/tmp/nonempty", true)
        .expect("create lower directory");
    lower
        .write_file("/tmp/nonempty/child.txt", b"child".to_vec())
        .expect("seed lower child");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .chmod("/tmp/nonempty", 0o700)
        .expect("copy up lower directory");

    assert_error_code(overlay.remove_dir("/tmp/nonempty"), "ENOTEMPTY");
    assert!(overlay.exists("/tmp/nonempty/child.txt"));
}

#[test]
fn overlay_rename_rejects_directory_trees_that_exceed_snapshot_depth_limit() {
    let lower = deep_directory_tree(1025);
    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    assert_error_code(overlay.rename("/deep", "/renamed"), "EINVAL");
}

#[test]
fn overlay_link_and_rename_preserve_upper_hardlinks_after_copy_up() {
    let mut lower = MemoryFileSystem::new();
    lower
        .write_file("/src.txt", b"base".to_vec())
        .expect("seed lower file");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .link("/src.txt", "/alias.txt")
        .expect("hardlink copied-up file");

    overlay
        .write_file("/alias.txt", b"mutated".to_vec())
        .expect("mutate linked file");
    assert_eq!(
        overlay.read_file("/src.txt").expect("read linked source"),
        b"mutated".to_vec()
    );

    overlay
        .rename("/src.txt", "/renamed.txt")
        .expect("rename hardlinked source");

    let alias_stat = overlay.stat("/alias.txt").expect("stat alias");
    let renamed_stat = overlay.stat("/renamed.txt").expect("stat renamed");
    assert_eq!(alias_stat.ino, renamed_stat.ino);
    assert_eq!(alias_stat.nlink, 2);
    assert_eq!(renamed_stat.nlink, 2);
    assert_eq!(
        overlay.read_file("/alias.txt").expect("read alias"),
        b"mutated".to_vec()
    );
    assert_eq!(
        overlay.read_file("/renamed.txt").expect("read renamed"),
        b"mutated".to_vec()
    );
    assert_error_code(overlay.read_file("/src.txt"), "ENOENT");
}

#[test]
fn root_filesystem_uses_bundled_base_and_round_trips_snapshots() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor::default())
        .expect("create default root");

    assert!(root.exists("/etc/os-release"));
    let os_release = root
        .lstat("/etc/os-release")
        .expect("lstat /etc/os-release");
    assert!(os_release.is_symbolic_link);
    assert_eq!(os_release.uid, 0);
    assert_eq!(os_release.gid, 0);
    let services = root
        .read_file("/etc/services")
        .expect("read bundled Linux services database");
    assert!(services
        .windows(b"smtp\t\t25/tcp\t\tmail".len())
        .any(|window| window == b"smtp\t\t25/tcp\t\tmail"));
    assert_eq!(
        root.read_file("/etc/hosts").expect("read bundled hosts file"),
        b"127.0.0.1 localhost localhost.localdomain\n::1 localhost localhost.localdomain ip6-localhost ip6-loopback\nfe00:: ip6-localnet\nff00:: ip6-mcastprefix\nff02::1 ip6-allnodes\nff02::2 ip6-allrouters\n127.0.1.1 agentos\n"
    );

    root.mkdir("/workspace", true).expect("create workspace");
    root.write_file("/workspace/run.sh", b"echo hi".to_vec())
        .expect("write bootstrapped file");

    let snapshot = root.snapshot().expect("snapshot root");
    let encoded = encode_snapshot(&snapshot).expect("encode root snapshot");
    let encoded_json: serde_json::Value =
        serde_json::from_slice(&encoded).expect("parse encoded snapshot");
    assert_eq!(
        encoded_json["format"],
        serde_json::json!(ROOT_FILESYSTEM_SNAPSHOT_FORMAT)
    );
    let decoded = decode_snapshot(&encoded).expect("decode root snapshot");

    assert!(decoded
        .entries
        .iter()
        .any(|entry| entry.path == "/etc/os-release"));
    assert!(decoded
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/run.sh"));
}

#[test]
fn higher_lowers_do_not_shadow_base_parent_directories_with_default_ownership() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: false,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![
                FilesystemEntry::directory("/etc/agentos"),
                FilesystemEntry::file("/bin/node", b"stub".to_vec()),
            ],
        }],
        bootstrap_entries: vec![],
    })
    .expect("create root");

    let bin = root.stat("/bin").expect("stat /bin");
    let etc = root.stat("/etc").expect("stat /etc");

    assert_eq!(bin.uid, 0);
    assert_eq!(bin.gid, 0);
    assert_eq!(etc.uid, 0);
    assert_eq!(etc.gid, 0);
}

#[test]
fn root_filesystem_composes_multiple_lowers_before_bootstrap_upper() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![
            RootFilesystemSnapshot {
                entries: vec![
                    FilesystemEntry::directory("/workspace"),
                    FilesystemEntry::file("/workspace/shared.txt", b"higher".to_vec()),
                    FilesystemEntry::file("/workspace/higher-only.txt", b"higher-only".to_vec()),
                ],
            },
            RootFilesystemSnapshot {
                entries: vec![
                    FilesystemEntry::directory("/workspace"),
                    FilesystemEntry::file("/workspace/shared.txt", b"lower".to_vec()),
                    FilesystemEntry::file("/workspace/lower-only.txt", b"lower-only".to_vec()),
                ],
            },
        ],
        bootstrap_entries: vec![
            FilesystemEntry::directory("/workspace"),
            FilesystemEntry::file("/workspace/shared.txt", b"upper".to_vec()),
            FilesystemEntry::file("/workspace/upper-only.txt", b"upper-only".to_vec()),
        ],
    })
    .expect("create multi-layer root");

    assert_eq!(
        root.read_file("/workspace/shared.txt")
            .expect("read upper override"),
        b"upper".to_vec()
    );
    assert_eq!(
        root.read_file("/workspace/higher-only.txt")
            .expect("read higher-only file"),
        b"higher-only".to_vec()
    );
    assert_eq!(
        root.read_file("/workspace/lower-only.txt")
            .expect("read lower-only file"),
        b"lower-only".to_vec()
    );
    assert_eq!(
        root.read_file("/workspace/upper-only.txt")
            .expect("read upper-only file"),
        b"upper-only".to_vec()
    );
}

#[test]
fn root_filesystem_bootstrap_suppresses_kernel_reserved_paths() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::directory("/workspace")],
        }],
        bootstrap_entries: vec![
            FilesystemEntry::directory("/dev/custom"),
            FilesystemEntry::file("/dev/null", b"not-a-device".to_vec()),
            FilesystemEntry::file("/proc/mounts", b"fake mounts".to_vec()),
            FilesystemEntry::file("/sys/kernel/info", b"blocked".to_vec()),
            FilesystemEntry::file("/workspace/allowed.txt", b"allowed".to_vec()),
        ],
    })
    .expect("create root with reserved bootstrap paths");

    assert!(!root.exists("/dev/custom"));
    assert!(!root.exists("/dev/null"));
    assert!(!root.exists("/proc/mounts"));
    assert!(!root.exists("/sys/kernel/info"));
    assert_eq!(
        root.read_file("/workspace/allowed.txt")
            .expect("read allowed bootstrap file"),
        b"allowed".to_vec()
    );

    let snapshot = root.snapshot().expect("snapshot root");
    assert!(snapshot
        .entries
        .iter()
        .all(|entry| !entry.path.starts_with("/dev/")));
    assert!(snapshot
        .entries
        .iter()
        .all(|entry| !entry.path.starts_with("/proc/")));
    assert!(snapshot
        .entries
        .iter()
        .all(|entry| !entry.path.starts_with("/sys/")));
    assert!(snapshot
        .entries
        .iter()
        .any(|entry| entry.path == "/workspace/allowed.txt"));
}

#[test]
fn snapshot_round_trip_preserves_file_type_bits_in_modes() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::Ephemeral,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::directory("/workspace")],
        }],
        bootstrap_entries: vec![],
    })
    .expect("create root");

    root.write_file("/workspace/file.txt", b"hello".to_vec())
        .expect("write file");
    root.mkdir("/workspace/subdir", false)
        .expect("create directory");
    root.symlink("/workspace/file.txt", "/workspace/link.txt")
        .expect("create symlink");

    let decoded = decode_snapshot(
        &encode_snapshot(&root.snapshot().expect("snapshot root")).expect("encode snapshot"),
    )
    .expect("decode snapshot");

    let file_entry = decoded
        .entries
        .iter()
        .find(|entry| entry.path == "/workspace/file.txt")
        .expect("file entry");
    assert_eq!(file_entry.mode & 0o170000, S_IFREG);

    let directory_entry = decoded
        .entries
        .iter()
        .find(|entry| entry.path == "/workspace/subdir")
        .expect("directory entry");
    assert_eq!(directory_entry.mode & 0o170000, S_IFDIR);

    let symlink_entry = decoded
        .entries
        .iter()
        .find(|entry| entry.path == "/workspace/link.txt")
        .expect("symlink entry");
    assert_eq!(symlink_entry.mode & 0o170000, S_IFLNK);
}

#[test]
fn decode_snapshot_accepts_zero_mode_strings() {
    let decoded = decode_snapshot(
        br#"{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": {
                "entries": [
                    {
                        "path": "/zero.txt",
                        "type": "file",
                        "mode": "0",
                        "uid": 0,
                        "gid": 0,
                        "content": "",
                        "encoding": "utf8"
                    },
                    {
                        "path": "/zero-dir",
                        "type": "directory",
                        "mode": "0000",
                        "uid": 0,
                        "gid": 0
                    }
                ]
            }
        }"#,
    )
    .expect("decode snapshot");

    let zero_file = decoded
        .entries
        .iter()
        .find(|entry| entry.path == "/zero.txt")
        .expect("zero file entry");
    assert_eq!(zero_file.mode, 0);

    let zero_dir = decoded
        .entries
        .iter()
        .find(|entry| entry.path == "/zero-dir")
        .expect("zero dir entry");
    assert_eq!(zero_dir.mode, 0);
}

#[test]
fn decode_snapshot_accepts_legacy_agentos_format() {
    let decoded = decode_snapshot(
        br#"{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": {
                "entries": [
                    {
                        "path": "/legacy.txt",
                        "type": "file",
                        "mode": "644",
                        "uid": 0,
                        "gid": 0,
                        "content": "legacy",
                        "encoding": "utf8"
                    }
                ]
            }
        }"#,
    )
    .expect("decode legacy snapshot");

    assert!(decoded
        .entries
        .iter()
        .any(|entry| entry.path == "/legacy.txt"));
}

#[test]
fn decode_snapshot_rejects_encoded_payloads_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(16),
        max_filesystem_bytes: Some(1024),
        max_inode_count: Some(16),
    };

    let error = decode_snapshot_with_import_limits(
        br#"{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": { "entries": [] }
        }"#,
        &limits,
    )
    .expect_err("oversized encoded snapshot should be rejected");

    assert!(error.to_string().contains("encoded bytes"));
}

#[test]
fn decode_snapshot_rejects_entry_counts_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(1024),
        max_inode_count: Some(1),
    };

    let error = decode_snapshot_with_import_limits(
        br#"{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": {
                "entries": [
                    {
                        "path": "/one",
                        "type": "directory",
                        "mode": "755",
                        "uid": 0,
                        "gid": 0
                    },
                    {
                        "path": "/two",
                        "type": "directory",
                        "mode": "755",
                        "uid": 0,
                        "gid": 0
                    }
                ]
            }
        }"#,
        &limits,
    )
    .expect_err("snapshot entry count should be rejected");

    assert!(error.to_string().contains("exceeding limit 1"));
}

#[test]
fn decode_snapshot_rejects_content_bytes_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(3),
        max_inode_count: Some(16),
    };

    let error = decode_snapshot_with_import_limits(
        br#"{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": {
                "entries": [
                    {
                        "path": "/large.txt",
                        "type": "file",
                        "mode": "644",
                        "uid": 0,
                        "gid": 0,
                        "content": "four",
                        "encoding": "utf8"
                    }
                ]
            }
        }"#,
        &limits,
    )
    .expect_err("snapshot content bytes should be rejected");

    assert!(error.to_string().contains("exceeding limit 3"));
}

#[test]
fn decode_snapshot_allows_metadata_heavy_entries_within_import_limits() {
    let path = format!("/{}", "a".repeat(4000));
    let snapshot = format!(
        r#"{{
            "format": "agentos_filesystem_snapshot_v1",
            "filesystem": {{
                "entries": [
                    {{
                        "path": "{path}",
                        "type": "file",
                        "mode": "644",
                        "uid": 0,
                        "gid": 0
                    }}
                ]
            }}
        }}"#
    );
    let limits = RootFilesystemImportLimits::from_resource_limits(&TestResourceLimits {
        max_filesystem_bytes: Some(0),
        max_inode_count: Some(1),
    });

    let decoded = decode_snapshot_with_import_limits(snapshot.as_bytes(), &limits)
        .expect("metadata-heavy empty file should fit decoded byte and inode limits");

    assert_eq!(decoded.entries.len(), 1);
    assert_eq!(decoded.entries[0].path, path);
}

#[test]
fn root_filesystem_rejects_descriptor_snapshots_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(3),
        max_inode_count: Some(16),
    };

    let error = RootFileSystem::from_descriptor_with_import_limits(
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: true,
            lowers: vec![RootFilesystemSnapshot {
                entries: vec![
                    FilesystemEntry::directory("/workspace"),
                    FilesystemEntry::file("/workspace/large.txt", b"four".to_vec()),
                ],
            }],
            bootstrap_entries: Vec::new(),
        },
        &limits,
    )
    .expect_err("descriptor snapshot content bytes should be rejected");

    assert!(error.to_string().contains("exceeding limit 3"));
}

#[test]
fn root_filesystem_rejects_implicit_parent_directories_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(16),
        max_inode_count: Some(1),
    };

    let error = RootFileSystem::from_descriptor_with_import_limits(
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: true,
            lowers: vec![RootFilesystemSnapshot {
                entries: vec![FilesystemEntry::file(
                    "/deep/nested/file.txt",
                    b"x".to_vec(),
                )],
            }],
            bootstrap_entries: Vec::new(),
        },
        &limits,
    )
    .expect_err("implicit parent directories should count against inode limits");

    assert!(error.to_string().contains("exceeding limit 1"));
}

#[test]
fn root_filesystem_rejects_duplicate_descriptor_entries_that_exceed_import_limits() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(16),
        max_inode_count: Some(1),
    };

    let error = RootFileSystem::from_descriptor_with_import_limits(
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: true,
            lowers: vec![RootFilesystemSnapshot {
                entries: vec![
                    FilesystemEntry::file("/dup.txt", Vec::new()),
                    FilesystemEntry::file("/dup.txt", Vec::new()),
                ],
            }],
            bootstrap_entries: Vec::new(),
        },
        &limits,
    )
    .expect_err("duplicate descriptor entries should count against import limits");

    assert!(error.to_string().contains("exceeding limit 1"));
}

#[test]
fn root_filesystem_normalizes_import_paths_before_creating_parent_directories() {
    let limits = RootFilesystemImportLimits {
        max_encoded_snapshot_bytes: Some(4096),
        max_filesystem_bytes: Some(16),
        max_inode_count: Some(2),
    };

    let mut root = RootFileSystem::from_descriptor_with_import_limits(
        RootFilesystemDescriptor {
            mode: RootFilesystemMode::Ephemeral,
            disable_default_base_layer: true,
            lowers: vec![RootFilesystemSnapshot {
                entries: vec![FilesystemEntry::file("/a/../b/file.txt", b"x".to_vec())],
            }],
            bootstrap_entries: Vec::new(),
        },
        &limits,
    )
    .expect("normalized import path should fit inode limit");

    assert!(!root.exists("/a"));
    assert!(root.exists("/b"));
    assert_eq!(
        root.read_file("/b/file.txt")
            .expect("read normalized import file"),
        b"x".to_vec()
    );
}

#[test]
fn read_only_root_locks_after_bootstrap_but_preserves_boot_entries() {
    let mut root = RootFileSystem::from_descriptor(RootFilesystemDescriptor {
        mode: RootFilesystemMode::ReadOnly,
        disable_default_base_layer: true,
        lowers: vec![RootFilesystemSnapshot {
            entries: vec![FilesystemEntry::directory("/workspace")],
        }],
        bootstrap_entries: vec![FilesystemEntry::file(
            "/workspace/boot.txt",
            b"ready".to_vec(),
        )],
    })
    .expect("create read-only root");

    root.finish_bootstrap();

    assert_eq!(
        root.read_file("/workspace/boot.txt")
            .expect("read preserved boot entry"),
        b"ready".to_vec()
    );
    assert_eq!(
        root.mkdir("/workspace", true)
            .expect("mkdir -p existing directory on readonly root"),
        ()
    );
    let error = root
        .write_file("/workspace/blocked.txt", b"blocked".to_vec())
        .expect_err("readonly root should reject new writes");
    assert_eq!(error.code(), "EROFS");
}
