#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        let unique = format!(
            "agent-os-which-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time before unix epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        fs::create_dir(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_fixture(path: &Path, mode: u32) {
    fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write fixture");
    let permissions = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, permissions).expect("set fixture permissions");
}

#[test]
fn which_skips_non_executable_path_entries() {
    let temp = TempDir::new();
    let fakebin = temp.path().join("fakebin");
    write_fixture(&fakebin, 0o644);

    let output = Command::new(env!("CARGO_BIN_EXE_which"))
        .arg("fakebin")
        .env("PATH", temp.path())
        .output()
        .expect("run which");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "unexpected stdout: {:?}", output.stdout);
}

#[test]
fn which_reports_executable_path_entries() {
    let temp = TempDir::new();
    let realbin = temp.path().join("realbin");
    write_fixture(&realbin, 0o755);

    let output = Command::new(env!("CARGO_BIN_EXE_which"))
        .arg("realbin")
        .env("PATH", temp.path())
        .output()
        .expect("run which");

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8(output.stdout).expect("utf8 stdout"), format!("{realbin}\n", realbin = realbin.display()));
}
