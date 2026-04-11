use std::path::{Path, PathBuf};

const NODE_BINARY_ENV: &str = "AGENT_OS_NODE_BINARY";
const DEFAULT_NODE_BINARY: &str = "node";

pub(crate) fn node_binary() -> String {
    let configured =
        std::env::var(NODE_BINARY_ENV).unwrap_or_else(|_| String::from(DEFAULT_NODE_BINARY));
    resolve_executable_path(&configured).unwrap_or(configured)
}

fn resolve_executable_path(binary: &str) -> Option<String> {
    let path = Path::new(binary);
    if path.is_absolute() || binary.contains(std::path::MAIN_SEPARATOR) {
        return Some(path.to_string_lossy().into_owned());
    }

    let path_env = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path_env) {
        let candidate: PathBuf = directory.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    None
}
