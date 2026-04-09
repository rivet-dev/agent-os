pub(crate) use crate::common::{encode_json_string_array, encode_json_string_map};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::unistd::{close, pipe2};
use serde::{Deserialize, Serialize};
use serde_json::from_str;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
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
const NODE_ALLOWED_BUILTINS_ENV: &str = "AGENT_OS_ALLOWED_NODE_BUILTINS";
const DANGEROUS_GUEST_ENV_KEYS: &[&str] = &[
    "DYLD_INSERT_LIBRARIES",
    "LD_LIBRARY_PATH",
    "LD_PRELOAD",
    "NODE_OPTIONS",
];
pub const NODE_CONTROL_PIPE_FD_ENV: &str = "AGENT_OS_CONTROL_PIPE_FD";
const RESERVED_CHILD_FD_MIN: RawFd = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeSignalDispositionAction {
    Default,
    Ignore,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSignalHandlerRegistration {
    pub action: NodeSignalDispositionAction,
    pub mask: Vec<u32>,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeControlMessage {
    NodeImportCacheMetrics {
        metrics: serde_json::Value,
    },
    PythonExit {
        #[serde(rename = "exitCode")]
        exit_code: i32,
    },
    SignalState {
        signal: u32,
        registration: NodeSignalHandlerRegistration,
    },
}

pub struct NodeControlChannel {
    pub parent_reader: File,
    pub child_writer: OwnedFd,
}

#[derive(Debug, Default)]
pub struct LinePrefixFilter {
    pending: Vec<u8>,
}

pub fn node_binary() -> String {
    let configured =
        std::env::var(NODE_BINARY_ENV).unwrap_or_else(|_| String::from(DEFAULT_NODE_BINARY));
    resolve_executable_path(&configured).unwrap_or(configured)
}

pub fn ensure_host_cwd_exists(cwd: &Path) -> std::io::Result<()> {
    fs::create_dir_all(cwd)
}

pub fn create_node_control_channel() -> std::io::Result<NodeControlChannel> {
    let (parent_reader, child_writer) = pipe2(OFlag::O_CLOEXEC).map_err(std::io::Error::other)?;

    Ok(NodeControlChannel {
        parent_reader: File::from(parent_reader),
        child_writer,
    })
}

#[derive(Debug, Default)]
pub(crate) struct ExportedChildFds {
    fds: Vec<RawFd>,
}

impl ExportedChildFds {
    pub(crate) fn export(
        &mut self,
        command: &mut Command,
        env_key: &str,
        source_fd: &OwnedFd,
    ) -> std::io::Result<RawFd> {
        let exported_fd = fcntl(
            source_fd.as_raw_fd(),
            FcntlArg::F_DUPFD(RESERVED_CHILD_FD_MIN),
        )
        .map_err(std::io::Error::other)?;
        command.env(env_key, exported_fd.to_string());
        self.fds.push(exported_fd);
        Ok(exported_fd)
    }
}

impl Drop for ExportedChildFds {
    fn drop(&mut self) {
        for fd in self.fds.drain(..) {
            let _ = close(fd);
        }
    }
}

pub fn configure_node_control_channel(
    command: &mut Command,
    child_writer: &OwnedFd,
    exported_fds: &mut ExportedChildFds,
) -> std::io::Result<()> {
    exported_fds.export(command, NODE_CONTROL_PIPE_FD_ENV, child_writer)?;
    Ok(())
}

pub fn harden_node_command(
    command: &mut Command,
    cwd: &Path,
    read_paths: &[PathBuf],
    write_paths: &[PathBuf],
    enable_permissions: bool,
    allow_wasi: bool,
    allow_worker: bool,
    allow_child_process: bool,
) {
    if enable_permissions {
        command.arg(NODE_PERMISSION_FLAG);
        if allow_worker {
            command.arg(NODE_ALLOW_WORKER_FLAG);
        }
        command.arg(NODE_DISABLE_SECURITY_WARNING_FLAG);
        if allow_wasi {
            command.arg(NODE_ALLOW_WASI_FLAG);
        }
        if allow_child_process {
            command.arg(NODE_ALLOW_CHILD_PROCESS_FLAG);
        }

        for path in
            allowed_paths(std::iter::once(cwd.to_path_buf()).chain(read_paths.iter().cloned()))
        {
            command.arg(format!("{NODE_ALLOW_FS_READ_FLAG}{}", path.display()));
        }

        for path in
            allowed_paths(std::iter::once(cwd.to_path_buf()).chain(write_paths.iter().cloned()))
        {
            command.arg(format!("{NODE_ALLOW_FS_WRITE_FLAG}{}", path.display()));
        }
    }

    command.env_clear();
}

pub fn env_builtin_enabled(env: &BTreeMap<String, String>, builtin: &str) -> bool {
    env.get(NODE_ALLOWED_BUILTINS_ENV)
        .and_then(|value| from_str::<Vec<String>>(value).ok())
        .is_some_and(|builtins| builtins.iter().any(|entry| entry == builtin))
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

pub fn spawn_node_control_reader<E, FM, FE>(
    reader: File,
    sender: Sender<E>,
    map_message: FM,
    map_error: FE,
) -> JoinHandle<()>
where
    E: Send + 'static,
    FM: Fn(NodeControlMessage) -> E + Send + 'static,
    FE: Fn(String) -> E + Send + 'static,
{
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    match serde_json::from_str::<NodeControlMessage>(trimmed) {
                        Ok(message) => {
                            if sender.send(map_message(message)).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            if sender
                                .send(map_error(format!(
                                    "invalid agent-os node control message: {error}\n"
                                )))
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(map_error(format!(
                        "agent-os node control read error: {error}\n"
                    )));
                    return;
                }
            }
        }
    })
}

impl LinePrefixFilter {
    pub fn filter_chunk(&mut self, chunk: &[u8], prefixes: &[&str]) -> Vec<u8> {
        self.pending.extend_from_slice(chunk);
        let mut filtered = Vec::new();

        while let Some(newline_index) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line = self.pending.drain(..=newline_index).collect::<Vec<_>>();
            if !has_control_prefix(&line, prefixes) {
                filtered.extend_from_slice(&line);
            }
        }

        filtered
    }
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

fn has_control_prefix(line: &[u8], prefixes: &[&str]) -> bool {
    let text = String::from_utf8_lossy(line);
    let trimmed = text.trim_end_matches(['\r', '\n']);
    prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::fcntl::FdFlag;
    use std::process::Command;

    #[test]
    fn exported_child_fds_use_reserved_high_numbers_while_sources_stay_cloexec() {
        let channel = create_node_control_channel().expect("create control channel");
        let source_fd = channel.child_writer.as_raw_fd();
        let source_flags = fcntl(channel.child_writer.as_raw_fd(), FcntlArg::F_GETFD)
            .expect("read source fd flags");

        assert!(
            FdFlag::from_bits_retain(source_flags).contains(FdFlag::FD_CLOEXEC),
            "child-side source fd should remain close-on-exec until it is remapped"
        );

        let mut command = Command::new("true");
        let mut exported_fds = ExportedChildFds::default();
        configure_node_control_channel(&mut command, &channel.child_writer, &mut exported_fds)
            .expect("export control fd");

        let exported_fd = command
            .get_envs()
            .find_map(|(key, value)| {
                (key == NODE_CONTROL_PIPE_FD_ENV)
                    .then(|| value.expect("exported fd env value"))
                    .and_then(|value| value.to_str())
                    .and_then(|value| value.parse::<RawFd>().ok())
            })
            .expect("control fd env");

        assert!(exported_fd >= RESERVED_CHILD_FD_MIN);
        assert_ne!(exported_fd, source_fd);
    }
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
    wait_for_streams_before_exit: bool,
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

        let _ = sender.send(exit_event(exit_code));

        if wait_for_streams_before_exit {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
        }
    });
}
