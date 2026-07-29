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
        let path = std::env::temp_dir().join(format!("cmd-sort-{name}-{nonce}"));
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

// The workspace patches ctrlc to the agentos stub, which reports
// ErrorKind::Unsupported for signal registration on every target. Forcing an
// external-sort spill into a temp directory exercises uu_sort's
// ensure_signal_handler_installed soft-skip path. Before the soft skip this
// invocation failed with exit code 2 and "failed to set up signal handler".
#[test]
fn external_sort_succeeds_without_signal_handler_support() {
    let dir = TestDir::new("spill");
    let input_path = dir.path().join("input.txt");
    let mut input = String::new();
    for i in (0..20_000u32).rev() {
        input.push_str(&format!("{i:08}\n"));
    }
    fs::write(&input_path, &input).expect("input should be written");

    let output = Command::new(env!("CARGO_BIN_EXE_sort"))
        .arg("-S")
        .arg("32K")
        .arg("-T")
        .arg(dir.path())
        .arg(&input_path)
        .output()
        .expect("sort should run");

    assert!(
        output.status.success(),
        "sort failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("output should be UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 20_000);
    assert_eq!(lines.first(), Some(&"00000000"));
    assert_eq!(lines.last(), Some(&"00019999"));
    assert!(lines.windows(2).all(|pair| pair[0] <= pair[1]));

    // The uutils_sort temp directory must be cleaned up by TempDir's Drop even
    // though no signal handler was installed.
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .expect("test dir should be readable")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("uutils_sort"))
        .collect();
    assert!(leftovers.is_empty(), "temp sort directory leaked: {leftovers:?}");
}
