use crate::adapter_common::stable_hash64;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

pub const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
pub const NODE_DISABLE_COMPILE_CACHE_ENV: &str = "NODE_DISABLE_COMPILE_CACHE";
pub const NODE_FROZEN_TIME_ENV: &str = "AGENTOS_FROZEN_TIME_MS";
pub const NODE_SANDBOX_ROOT_ENV: &str = "AGENTOS_SANDBOX_ROOT";

pub fn env_flag_enabled(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| value == "1")
}

pub fn resolve_execution_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub fn warmup_marker_path(
    marker_dir: &Path,
    prefix: &str,
    version: &str,
    contents: &str,
) -> PathBuf {
    marker_dir.join(format!(
        "{prefix}-v{version}-{:016x}.stamp",
        stable_hash64(contents.as_bytes())
    ))
}

pub fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) => format!(
            "{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            metadata.mtime(),
            metadata.mtime_nsec(),
        ),
        Err(_) => String::from("missing"),
    }
}

#[cfg(test)]
mod tests {
    use super::file_fingerprint;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    #[test]
    fn file_fingerprint_tracks_inode_and_mutation_time() {
        let temp = tempdir().expect("create temp dir");
        let path = temp.path().join("module.wasm");

        fs::write(&path, b"first").expect("write wasm file");
        let metadata = fs::metadata(&path).expect("stat wasm file");
        let first = file_fingerprint(&path);

        assert_eq!(
            first,
            format!(
                "{}:{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            )
        );

        std::thread::sleep(std::time::Duration::from_millis(25));
        fs::write(&path, b"second").expect("overwrite wasm file");

        assert_ne!(
            file_fingerprint(&path),
            first,
            "rewriting a tracked asset in place must invalidate warmup markers"
        );
    }
}
