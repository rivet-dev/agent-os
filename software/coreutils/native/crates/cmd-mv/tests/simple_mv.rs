use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cmd-mv-{name}-{nonce}"));
        fs::create_dir(&path).expect("test dir should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_mv(args: &[&Path]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mv"));
    for arg in args {
        command.arg(arg);
    }
    command.output().expect("mv should run")
}

#[test]
fn same_source_and_destination_does_not_delete_file() {
    let dir = TestDir::new("same-file");
    let file = dir.path().join("file.txt");
    fs::write(&file, "still here").expect("file should be written");

    let output = run_mv(&[&file, &file]);

    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(&file).expect("source should still exist"),
        "still here"
    );
}

#[cfg(unix)]
#[test]
fn same_filesystem_move_preserves_inode() {
    use std::os::unix::fs::MetadataExt;

    let dir = TestDir::new("preserve-inode");
    let source = dir.path().join("source.txt");
    let destination = dir.path().join("destination.txt");
    fs::write(&source, "payload").expect("source file should be written");
    let source_inode = fs::metadata(&source).expect("source metadata").ino();

    let output = run_mv(&[&source, &destination]);

    assert!(output.status.success());
    assert!(!source.exists());
    assert_eq!(
        fs::metadata(&destination)
            .expect("destination metadata")
            .ino(),
        source_inode
    );
}

#[test]
fn nonempty_directory_destination_reports_the_destination_without_host_errno_numbers() {
    let dir = TestDir::new("nonempty-destination");
    let source = dir.path().join("source");
    let target_root = dir.path().join("target");
    let destination = target_root.join("source");
    fs::create_dir(&source).expect("source dir should be created");
    fs::create_dir(&target_root).expect("target root should be created");
    fs::create_dir(&destination).expect("destination dir should be created");
    fs::write(destination.join("existing"), "payload").expect("destination should be nonempty");

    let output = run_mv(&[&source, &target_root]);
    let stderr = String::from_utf8(output.stderr).expect("mv stderr should be UTF-8");

    assert!(!output.status.success());
    assert!(
        stderr.contains(&format!("cannot overwrite '{}'", destination.display())),
        "unexpected stderr: {stderr}"
    );
    assert!(!stderr.contains("(os error"), "unexpected stderr: {stderr}");
}

#[cfg(unix)]
#[test]
fn rejects_destination_inside_source_through_symlink() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new("symlink-child");
    let source = dir.path().join("source");
    let link = dir.path().join("link");
    fs::create_dir(&source).expect("source dir should be created");
    fs::write(source.join("file.txt"), "payload").expect("source file should be written");
    symlink(&source, &link).expect("symlink should be created");

    let output = run_mv(&[&source, &link.join("child")]);

    assert!(!output.status.success());
    assert!(source.join("file.txt").exists());
    assert!(!source.join("child").exists());
}

#[cfg(unix)]
#[test]
fn moves_dangling_symlink() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new("dangling-symlink");
    let source = dir.path().join("source-link");
    let destination = dir.path().join("destination-link");
    symlink("missing-target", &source).expect("dangling symlink should be created");

    let output = run_mv(&[&source, &destination]);

    assert!(output.status.success());
    assert!(!source.exists());
    assert_eq!(
        fs::read_link(&destination).expect("destination should be a symlink"),
        PathBuf::from("missing-target")
    );
}

#[cfg(unix)]
#[test]
fn allows_lexically_same_path_that_crosses_symlink_parent() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new("symlink-parent");
    let base = dir.path().join("base");
    let target = dir.path().join("target");
    let inner = target.join("inner");
    fs::create_dir(&base).expect("base dir should be created");
    fs::create_dir_all(&inner).expect("target inner dir should be created");
    fs::write(base.join("src"), "base").expect("base file should be written");
    fs::write(target.join("src"), "target").expect("target file should be written");
    symlink(&inner, base.join("link")).expect("symlink should be created");

    let output = run_mv(&[&base.join("link").join("..").join("src"), &base.join("src")]);

    assert!(output.status.success());
    assert_eq!(
        fs::read_to_string(base.join("src")).expect("destination should exist"),
        "target"
    );
    assert!(!target.join("src").exists());
}

#[test]
fn allows_destination_sibling_via_parent_component() {
    let dir = TestDir::new("parent-component");
    let base = dir.path().join("base");
    let source = base.join("src");
    fs::create_dir(&base).expect("base dir should be created");
    fs::create_dir(&source).expect("source dir should be created");
    fs::write(source.join("file.txt"), "payload").expect("source file should be written");

    let output = run_mv(&[&source, &source.join("..").join("dst")]);

    assert!(output.status.success());
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(base.join("dst").join("file.txt"))
            .expect("destination file should exist"),
        "payload"
    );
}

#[cfg(unix)]
#[test]
fn allows_destination_under_symlink_that_points_outside_source() {
    use std::os::unix::fs::symlink;

    let dir = TestDir::new("outside-link");
    let source = dir.path().join("source");
    let outside = dir.path().join("outside");
    fs::create_dir(&source).expect("source dir should be created");
    fs::create_dir(&outside).expect("outside dir should be created");
    fs::write(source.join("file.txt"), "payload").expect("source file should be written");
    symlink(&outside, source.join("link")).expect("symlink should be created");

    let output = run_mv(&[&source, &source.join("link").join("dst")]);

    assert!(output.status.success());
    assert!(!source.exists());
    assert_eq!(
        fs::read_to_string(outside.join("dst").join("file.txt"))
            .expect("destination file should exist"),
        "payload"
    );
}
