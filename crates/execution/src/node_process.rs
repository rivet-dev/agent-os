pub(crate) use crate::common::{encode_json_string_array, encode_json_string_map};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

const NODE_BINARY_ENV: &str = "AGENT_OS_NODE_BINARY";
const DEFAULT_NODE_BINARY: &str = "node";
const NODE_PERMISSION_FLAG: &str = "--permission";
const NODE_ALLOW_WASI_FLAG: &str = "--allow-wasi";
const NODE_ALLOW_WORKER_FLAG: &str = "--allow-worker";
const NODE_ALLOW_CHILD_PROCESS_FLAG: &str = "--allow-child-process";
const NODE_DISABLE_SECURITY_WARNING_FLAG: &str = "--disable-warning=SecurityWarning";
const NODE_ALLOW_FS_READ_FLAG: &str = "--allow-fs-read=";
const NODE_ALLOW_FS_WRITE_FLAG: &str = "--allow-fs-write=";
const DANGEROUS_GUEST_ENV_KEYS: &[&str] = &[
    "DYLD_INSERT_LIBRARIES",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NODE_OPTIONS",
];

pub fn node_binary() -> String {
    let configured =
        std::env::var(NODE_BINARY_ENV).unwrap_or_else(|_| String::from(DEFAULT_NODE_BINARY));
    resolve_executable_path(&configured).unwrap_or(configured)
}

pub fn harden_node_command(
    command: &mut Command,
    cwd: &Path,
    read_paths: &[PathBuf],
    write_paths: &[PathBuf],
    allow_wasi: bool,
    allow_child_process: bool,
) {
    command.arg(NODE_PERMISSION_FLAG);
    command.arg(NODE_ALLOW_WORKER_FLAG);
    command.arg(NODE_DISABLE_SECURITY_WARNING_FLAG);
    if allow_wasi {
        command.arg(NODE_ALLOW_WASI_FLAG);
    }
    if allow_child_process {
        command.arg(NODE_ALLOW_CHILD_PROCESS_FLAG);
    }

    for path in allowed_paths(std::iter::once(cwd.to_path_buf()).chain(read_paths.iter().cloned()))
    {
        command.arg(format!("{NODE_ALLOW_FS_READ_FLAG}{}", path.display()));
    }

    for path in allowed_paths(std::iter::once(cwd.to_path_buf()).chain(write_paths.iter().cloned()))
    {
        command.arg(format!("{NODE_ALLOW_FS_WRITE_FLAG}{}", path.display()));
    }

    command.env_clear();
}

pub fn node_resolution_read_paths(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for root in roots {
        let mut current = root.as_path();
        loop {
            let package_json = current.join("package.json");
            if package_json.is_file() {
                paths.push(package_json);
            }

            let node_modules = current.join("node_modules");
            if node_modules.is_dir() {
                paths.push(node_modules);
            }

            let Some(parent) = current.parent() else {
                break;
            };
            if parent == current {
                break;
            }
            current = parent;
        }
    }

    paths
}

pub fn apply_guest_env(
    command: &mut Command,
    env: &BTreeMap<String, String>,
    reserved_keys: &[&str],
) {
    for (key, value) in env {
        if reserved_keys.contains(&key.as_str()) || DANGEROUS_GUEST_ENV_KEYS.contains(&key.as_str())
        {
            continue;
        }
        command.env(key, value);
    }
}

pub fn resolve_path_like_specifier(cwd: &Path, specifier: &str) -> Option<PathBuf> {
    if specifier.starts_with("file://") {
        return Some(PathBuf::from(specifier.trim_start_matches("file://")));
    }
    if specifier.starts_with("file:") {
        return Some(PathBuf::from(specifier.trim_start_matches("file:")));
    }
    if specifier.starts_with('/') {
        return Some(PathBuf::from(specifier));
    }
    if specifier.starts_with("./") || specifier.starts_with("../") {
        return Some(cwd.join(specifier));
    }

    None
}

pub fn spawn_stream_reader<E, R, F>(
    mut reader: R,
    sender: Sender<E>,
    map_event: F,
) -> JoinHandle<()>
where
    E: Send + 'static,
    R: Read + Send + 'static,
    F: Fn(Vec<u8>) -> E + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 1024];

        loop {
            match reader.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    if sender.send(map_event(buffer[..read].to_vec())).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    })
}
fn allowed_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    let mut seen = BTreeSet::new();

    for path in paths {
        let normalized = normalize_path(path);
        let key = normalized.to_string_lossy().into_owned();
        if seen.insert(key) {
            unique.push(normalized);
        }
    }

    unique
}

fn normalize_path(path: PathBuf) -> PathBuf {
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };

    absolute.canonicalize().unwrap_or(absolute)
}

fn resolve_executable_path(binary: &str) -> Option<String> {
    let path = Path::new(binary);
    if path.is_absolute() || binary.contains(std::path::MAIN_SEPARATOR) {
        return Some(path.to_string_lossy().into_owned());
    }

    let path_env = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path_env) {
        let candidate = directory.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }

    None
}

pub fn spawn_waiter<E, FE, FW>(
    mut child: Child,
    stdout_reader: JoinHandle<()>,
    stderr_reader: JoinHandle<()>,
    sender: Sender<E>,
    exit_event: FE,
    wait_error_event: FW,
) where
    E: Send + 'static,
    FE: Fn(i32) -> E + Send + 'static,
    FW: Fn(String) -> E + Send + 'static,
{
    thread::spawn(move || {
        let exit_code = match child.wait() {
            Ok(status) => status.code().unwrap_or(1),
            Err(err) => {
                let _ = sender.send(wait_error_event(format!(
                    "agent-os execution wait error: {err}\n"
                )));
                1
            }
        };

        let _ = stdout_reader.join();
        let _ = stderr_reader.join();
        let _ = sender.send(exit_event(exit_code));
    });
}
