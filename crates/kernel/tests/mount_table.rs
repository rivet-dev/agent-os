use agent_os_kernel::mount_table::{MountOptions, MountTable};
use agent_os_kernel::vfs::{MemoryFileSystem, VirtualFileSystem};

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
fn mount_table_rejects_symlinks_that_cross_mount_boundaries() {
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
        .symlink("../root.txt", "/mounted/root-link")
        .expect_err("cross-mount symlink should fail");
    assert_eq!(error.code(), "EXDEV");
}
