use agentos_vfs_core::posix::{SingleSymlinkFileSystem, VirtualFileSystem};

#[test]
fn single_symlink_filesystem_exposes_root_symlink_only() {
    let fs = SingleSymlinkFileSystem::new("../pkgs/pi/current/bin/pi");

    let stat = fs.lstat("/").expect("lstat root symlink");
    assert!(stat.is_symbolic_link);
    assert!(!stat.is_directory);
    assert_eq!(stat.mode & 0o777, 0o777);
    assert_eq!(
        fs.read_link("/").expect("read root symlink target"),
        "../pkgs/pi/current/bin/pi"
    );

    let error = fs
        .lstat("/anything")
        .expect_err("non-root paths do not exist");
    assert_eq!(error.code(), "ENOENT");
}
