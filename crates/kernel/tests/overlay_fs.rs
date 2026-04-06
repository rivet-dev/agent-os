use agent_os_kernel::overlay_fs::{OverlayFileSystem, OverlayMode};
use agent_os_kernel::vfs::{MemoryFileSystem, VfsError, VirtualFileSystem};

fn assert_error_code<T: std::fmt::Debug>(result: Result<T, VfsError>, expected: &str) {
    let error = result.expect_err("expected operation to fail");
    assert_eq!(error.code(), expected);
}

#[test]
fn delete_on_lower_file_creates_whiteout_and_filters_directory_entries() {
    let mut lower = MemoryFileSystem::new();
    lower.mkdir("/etc", true).expect("create lower /etc");
    lower
        .write_file("/etc/base.txt", b"base".to_vec())
        .expect("seed lower file");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .remove_file("/etc/base.txt")
        .expect("whiteout lower file");

    assert!(!overlay.exists("/etc/base.txt"));
    assert_error_code(overlay.read_file("/etc/base.txt"), "ENOENT");
    assert_eq!(
        overlay.read_dir("/etc").expect("read merged directory"),
        Vec::<String>::new()
    );
}

#[test]
fn copying_up_directory_marks_it_opaque_and_masks_lower_children() {
    let mut lower = MemoryFileSystem::new();
    lower.mkdir("/data", true).expect("create lower directory");
    lower
        .write_file("/data/base.txt", b"base".to_vec())
        .expect("seed lower file");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .chmod("/data", 0o700)
        .expect("copy up lower directory");
    overlay
        .write_file("/data/upper.txt", b"upper".to_vec())
        .expect("write upper-only file");

    assert_eq!(
        overlay.read_dir("/data").expect("read opaque directory"),
        vec![String::from("upper.txt")]
    );
    assert_eq!(
        overlay.read_file("/data/upper.txt").expect("read upper file"),
        b"upper".to_vec()
    );
}

#[test]
fn writes_copy_up_lower_files_without_mutating_read_only_lower_layers() {
    let mut lower = MemoryFileSystem::new();
    lower.mkdir("/config", true).expect("create lower directory");
    lower
        .write_file("/config/app.json", br#"{"mode":"lower"}"#.to_vec())
        .expect("seed lower file");
    let lower_snapshot = lower.snapshot();

    let mut overlay = OverlayFileSystem::new(
        vec![MemoryFileSystem::from_snapshot(lower_snapshot.clone())],
        OverlayMode::Ephemeral,
    );
    overlay
        .write_file("/config/app.json", br#"{"mode":"upper"}"#.to_vec())
        .expect("mutate merged file");

    assert_eq!(
        overlay
            .read_file("/config/app.json")
            .expect("read copied-up file"),
        br#"{"mode":"upper"}"#.to_vec()
    );

    let mut original_lower = MemoryFileSystem::from_snapshot(lower_snapshot);
    assert_eq!(
        original_lower
            .read_file("/config/app.json")
            .expect("read original lower file"),
        br#"{"mode":"lower"}"#.to_vec()
    );
}

#[test]
fn cross_layer_rename_moves_directory_tree_without_partial_state() {
    let mut lower = MemoryFileSystem::new();
    lower
        .mkdir("/src/nested", true)
        .expect("create lower directory tree");
    lower
        .write_file("/src/root.txt", b"root".to_vec())
        .expect("seed root child");
    lower
        .write_file("/src/nested/child.txt", b"nested".to_vec())
        .expect("seed nested child");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .rename("/src", "/dst")
        .expect("rename lower directory");

    assert_eq!(
        overlay.read_dir("/").expect("read root after rename"),
        vec![String::from("dst")]
    );
    assert_eq!(
        overlay.read_file("/dst/root.txt").expect("read renamed root child"),
        b"root".to_vec()
    );
    assert_eq!(
        overlay
            .read_file("/dst/nested/child.txt")
            .expect("read renamed nested child"),
        b"nested".to_vec()
    );
    assert_error_code(overlay.read_dir("/src"), "ENOENT");
}

#[test]
fn overlay_metadata_root_is_filtered_from_user_visible_results() {
    let mut lower = MemoryFileSystem::new();
    lower.mkdir("/data", true).expect("create lower /data");
    lower
        .write_file("/data/base.txt", b"base".to_vec())
        .expect("seed lower file");
    lower.mkdir("/logs", true).expect("create lower /logs");

    let mut overlay = OverlayFileSystem::new(vec![lower], OverlayMode::Ephemeral);
    overlay
        .remove_file("/data/base.txt")
        .expect("create whiteout marker");
    overlay
        .chmod("/logs", 0o700)
        .expect("create opaque marker");

    assert!(!overlay.exists("/.agent-os-overlay"));
    assert_eq!(
        overlay.read_dir("/").expect("read filtered root"),
        vec![String::from("data"), String::from("logs")]
    );
    assert_error_code(overlay.lstat("/.agent-os-overlay"), "ENOENT");
    assert_error_code(overlay.read_dir("/.agent-os-overlay"), "ENOENT");
}
