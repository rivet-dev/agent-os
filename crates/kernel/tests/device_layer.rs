use agent_os_kernel::device_layer::create_device_layer;
use agent_os_kernel::vfs::{MemoryFileSystem, VfsResult, VirtualFileSystem};
use std::fmt::Debug;

fn assert_error_code<T: Debug>(result: VfsResult<T>, expected: &str) {
    let error = result.expect_err("operation should fail");
    assert_eq!(error.code(), expected);
}

fn create_test_vfs() -> impl VirtualFileSystem {
    create_device_layer(MemoryFileSystem::new())
}

fn assert_not_trivial_pattern(bytes: &[u8]) {
    assert!(bytes.iter().any(|byte| *byte != 0));
    assert!(
        bytes.windows(2).any(|window| window[0] != window[1]),
        "random data should not collapse to a repeated byte"
    );

    let first_step = bytes[1].wrapping_sub(bytes[0]);
    assert!(
        bytes
            .windows(2)
            .any(|window| window[1].wrapping_sub(window[0]) != first_step),
        "random data should not look like a simple arithmetic progression"
    );
}

#[test]
fn special_devices_expose_expected_read_and_write_behavior() {
    let mut filesystem = create_test_vfs();

    assert_eq!(
        filesystem
            .read_file("/dev/null")
            .expect("read /dev/null")
            .len(),
        0
    );

    filesystem
        .write_file("/dev/zero", "ignored")
        .expect("write /dev/zero");
    let zeroes = filesystem
        .pread("/dev/zero", 0, 5)
        .expect("pread 5 bytes from /dev/zero");
    assert_eq!(zeroes.len(), 5);
    assert!(zeroes.iter().all(|byte| *byte == 0));

    let first = filesystem
        .pread("/dev/urandom", 0, 1024)
        .expect("pread /dev/urandom");
    let second = filesystem
        .pread("/dev/urandom", 0, 1024 * 1024)
        .expect("pread 1MiB from /dev/urandom");
    assert_eq!(first.len(), 1024);
    assert_eq!(second.len(), 1024 * 1024);
    assert_not_trivial_pattern(&first);
    assert_not_trivial_pattern(&second[..1024]);
    assert_ne!(first, second);
}

#[test]
fn device_paths_exist_and_stat_as_devices() {
    let mut filesystem = create_test_vfs();

    for path in [
        "/dev/null",
        "/dev/zero",
        "/dev/stdin",
        "/dev/stdout",
        "/dev/stderr",
        "/dev/urandom",
        "/dev",
    ] {
        assert!(filesystem.exists(path), "{path} should exist");
    }

    let device_stat = filesystem.stat("/dev/null").expect("stat /dev/null");
    assert!(!device_stat.is_directory);
    assert_eq!(device_stat.mode, 0o666);

    let dir_stat = filesystem.stat("/dev").expect("stat /dev");
    assert!(dir_stat.is_directory);
    assert_eq!(dir_stat.mode, 0o755);
}

#[test]
fn readdir_lists_known_device_entries() {
    let mut filesystem = create_test_vfs();
    let entries = filesystem.read_dir("/dev").expect("read /dev");

    assert!(entries.contains(&String::from("null")));
    assert!(entries.contains(&String::from("zero")));
    assert!(entries.contains(&String::from("stdin")));
    assert!(entries.contains(&String::from("fd")));
}

#[test]
fn stdio_devices_fall_through_to_backing_vfs_for_regular_io() {
    let mut filesystem = create_test_vfs();

    assert_error_code(filesystem.read_file("/dev/stdin"), "ENOENT");
    assert_error_code(filesystem.read_file("/dev/stdout"), "ENOENT");
    assert_error_code(filesystem.read_file("/dev/stderr"), "ENOENT");

    filesystem
        .write_file("/dev/stdout", "output")
        .expect("write /dev/stdout");
    filesystem
        .write_file("/dev/stderr", "error output")
        .expect("write /dev/stderr");

    assert_eq!(
        filesystem
            .read_text_file("/dev/stdout")
            .expect("read /dev/stdout"),
        "output"
    );
    assert_eq!(
        filesystem
            .read_text_file("/dev/stderr")
            .expect("read /dev/stderr"),
        "error output"
    );
}

#[test]
fn mutating_device_paths_fails_closed_or_noops_like_the_legacy_layer() {
    let mut filesystem = create_test_vfs();
    filesystem
        .write_file("/tmp/a.txt", "data")
        .expect("write regular file");

    assert_error_code(filesystem.remove_file("/dev/null"), "EPERM");
    assert_error_code(filesystem.rename("/dev/null", "/tmp/x"), "EPERM");
    assert_error_code(filesystem.rename("/tmp/a.txt", "/dev/null"), "EPERM");
    assert_error_code(filesystem.link("/dev/null", "/tmp/devlink"), "EPERM");

    filesystem
        .truncate("/dev/null", 0)
        .expect("truncate /dev/null");
    assert_eq!(
        filesystem
            .read_file("/dev/null")
            .expect("read /dev/null")
            .len(),
        0
    );
}

#[test]
fn realpath_and_non_device_passthrough_match_legacy_behavior() {
    let mut filesystem = create_test_vfs();

    assert_eq!(
        filesystem
            .realpath("/dev/null")
            .expect("realpath /dev/null"),
        "/dev/null"
    );
    assert_eq!(
        filesystem.realpath("/dev/fd").expect("realpath /dev/fd"),
        "/dev/fd"
    );

    filesystem
        .write_file("/tmp/test.txt", "hello")
        .expect("write regular file");
    assert_eq!(
        filesystem
            .read_text_file("/tmp/test.txt")
            .expect("read regular file"),
        "hello"
    );
}
