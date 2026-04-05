use crate::common::stable_hash64;
use crate::node_import_cache::NodeImportCache;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const NODE_COMPILE_CACHE_ENV: &str = "NODE_COMPILE_CACHE";
pub(crate) const NODE_DISABLE_COMPILE_CACHE_ENV: &str = "NODE_DISABLE_COMPILE_CACHE";
pub(crate) const NODE_FROZEN_TIME_ENV: &str = "AGENT_OS_FROZEN_TIME_MS";
pub(crate) const NODE_SANDBOX_ROOT_ENV: &str = "AGENT_OS_SANDBOX_ROOT";

pub(crate) fn env_flag_enabled(env: &BTreeMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| value == "1")
}

pub(crate) fn sandbox_root(env: &BTreeMap<String, String>, cwd: &Path) -> PathBuf {
    env.get(NODE_SANDBOX_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf())
}

pub(crate) fn import_cache_root(import_cache: &NodeImportCache, fallback: &Path) -> PathBuf {
    import_cache
        .cache_path()
        .parent()
        .unwrap_or(fallback)
        .to_path_buf()
}

pub(crate) fn configure_compile_cache(
    command: &mut Command,
    compile_cache_dir: &Path,
) -> Result<(), io::Error> {
    fs::create_dir_all(compile_cache_dir)?;
    command
        .env_remove(NODE_DISABLE_COMPILE_CACHE_ENV)
        .env(NODE_COMPILE_CACHE_ENV, compile_cache_dir);
    Ok(())
}

pub(crate) fn compile_cache_ready(compile_cache_dir: &Path) -> bool {
    fs::read_dir(compile_cache_dir)
        .ok()
        .and_then(|mut entries| entries.next())
        .is_some()
}

pub(crate) fn resolve_execution_path(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

pub(crate) fn warmup_marker_path(
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

pub(crate) fn file_fingerprint(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(metadata) => format!("{}:{}", metadata.dev(), metadata.ino()),
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
    fn file_fingerprint_uses_inode_identity() {
        let temp = tempdir().expect("create temp dir");
        let path = temp.path().join("module.wasm");

        fs::write(&path, b"first").expect("write wasm file");
        let metadata = fs::metadata(&path).expect("stat wasm file");

        assert_eq!(
            file_fingerprint(&path),
            format!("{}:{}", metadata.dev(), metadata.ino())
        );
    }
}
