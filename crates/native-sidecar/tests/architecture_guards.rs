//! Architecture / boundary guards (CI hardening, item #2).
//!
//! This is a *chokepoint lint*: it scans the AgentOS Rust source tree and
//! FAILS if a security-sensitive host API ("banned API") appears OUTSIDE an
//! explicit allowlist of sanctioned modules. The goal is to keep host access
//! funnelled through a small, reviewable set of files so that a NEW use of
//! `std::fs`, raw sockets, `Command::new`, or process-environment reads cannot
//! be introduced without either landing in a sanctioned module or consciously
//! updating this allowlist (which forces review of the boundary).
//!
//! The four banned classes mirror the kernel/sidecar trust boundary:
//!
//!   * fs      -- `std::fs` / `tokio::fs` / `File::open` / `File::create` /
//!     `OpenOptions` / raw `openat`. Sanctioned only in the sidecar host-FS
//!     plumbing, the VFS-backed runtime modules, and runtime asset/module
//!     loaders.
//!   * net     -- `std::net` / `tokio::net` socket constructors, `reqwest`,
//!     `hyper`, `to_socket_addrs`, `UnixStream::pair`. Sanctioned only in the
//!     kernel DNS/socket plane, the sidecar host-net chokepoint
//!     (`sidecar::execution`), the embedded V8 runtime IPC pair, and
//!     host-backed storage plugins.
//!   * process -- `std::process::Command` / `tokio::process` / OS `fork`.
//!     Sanctioned only where agentos spawns its own helper process (the
//!     client transport that launches the sidecar). Guest "process" spawns are
//!     dispatched through the kernel `CommandDriver` registry and never touch
//!     `Command::new`.
//!   * env     -- `std::env::var` / `var_os` / `vars`. Sanctioned only at the
//!     scrubbed env-assembly / bootstrap points that read host configuration
//!     before a VM is constructed.
//!
//! IMPORTANT MAINTENANCE NOTES
//! ---------------------------
//! * The allowlist is built from the CURRENT legitimate uses so the test is
//!   GREEN today; it is designed to catch only *new* uses.
//! * Build scripts (`build.rs`, `*_build_support.rs`, ...), `tests/` and
//!   `benches/` directories, and inline `#[cfg(test)]` modules are excluded
//!   from the scan (they are not production host-access surface).
//! * `crates/execution/src/benchmark.rs`, `crates/execution/src/bin/`, and
//!   `crates/native-baseline/` hold benchmarking/dev tooling and are excluded
//!   for the same reason.
//!
//! If you are adding a genuinely new sanctioned chokepoint, add its
//! repo-relative path to the relevant allowlist below WITH a comment
//! explaining why the host access is safe. If you are adding host access
//! anywhere else, route it through an existing chokepoint instead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Repo root = `<root>/crates/native-sidecar` -> up two levels.
fn repo_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("sidecar crate should live two levels under the repo root")
        .to_path_buf()
}

#[test]
fn unix_listener_close_is_lossless_and_acknowledged() {
    let root = repo_root();
    let unix =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/network/unix.rs"))
            .expect("read Unix reactor source");
    let rpc =
        std::fs::read_to_string(root.join("crates/native-sidecar/src/execution/javascript/rpc.rs"))
            .expect("read JavaScript RPC source");
    let compact_unix: String = unix
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let compact_rpc: String = rpc
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();

    assert!(
        compact_unix.contains("self.close_notify.notify_one();")
            && compact_unix.contains("self.close_completion")
            && !compact_unix.contains("self.close_notify.notify_waiters()"),
        "Unix listener close must retain a notification permit between acceptor select points"
    );
    assert!(
        compact_unix.contains("UnixListenerTaskCompletion(Some(close_complete))")
            && compact_unix.contains("completion.send(())"),
        "the Unix listener owner must acknowledge every terminal path after dropping its FD"
    );
    assert!(
        compact_rpc.contains("tokio::time::timeout(operation_deadline,close_completion).await")
            && compact_rpc.contains("JavascriptSyncRpcServiceResponse::Deferred"),
        "the listener close bridge response must await bounded owner-task completion"
    );
}

/// Every production Rust source file under `crates/*/src/`, repo-relative,
/// excluding build scripts, benches, bins, and `tests/` trees.
fn production_source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let crates_dir = root.join("crates");
    let mut crate_dirs: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .expect("crates/ directory should exist")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    crate_dirs.sort();
    for crate_dir in crate_dirs {
        let src = crate_dir.join("src");
        if src.is_dir() {
            collect_rs(&src, root, &mut out);
        }
    }
    out.sort();
    out
}

fn collect_rs(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("read_dir {dir:?}: {err}"))
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            // Exclude bench/dev binaries that are not production runtime.
            if path.file_name().map(|n| n == "bin").unwrap_or(false) {
                continue;
            }
            collect_rs(&path, root, out);
        } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let rel = path
                .strip_prefix(root)
                .expect("source path under repo root")
                .to_path_buf();
            out.push(rel);
        }
    }
}

/// Returns true if the file is excluded from scanning entirely.
fn is_excluded_file(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    s.ends_with("build.rs")
        || s.ends_with("build_support.rs")
        || s.ends_with("v8_bridge_build.rs")
        // Benchmarking / dev tooling, not production host-access surface.
        || s == "crates/execution/src/benchmark.rs"
        || s.starts_with("crates/native-baseline/")
        // Browser support is intentionally retained but disabled; dormant
        // browser sources must not gate the native reactor migration.
        || s.starts_with("crates/native-sidecar-browser/")
        || s.starts_with("crates/agentos-sidecar-browser/")
        || s.contains("/src/bin/")
}

/// Strip a trailing `//` line comment (good enough for this lint; we are not
/// trying to be a full Rust parser, only to avoid flagging commented examples).
fn strip_line_comment(line: &str) -> &str {
    match line.find("//") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Track whether a line is inside a top-level `#[cfg(test)]` module so test
/// code is excluded from the scan. We watch for `#[cfg(test)]` immediately
/// followed by a `mod ... {` and then balance braces until the module closes.
struct CfgTestTracker {
    pending_cfg_test: bool,
    depth: u32,
}

impl CfgTestTracker {
    fn new() -> Self {
        Self {
            pending_cfg_test: false,
            depth: 0,
        }
    }

    /// Feed a line. Returns true if this line is inside a `#[cfg(test)]` module.
    fn in_test(&mut self, raw: &str) -> bool {
        let line = strip_line_comment(raw);
        let trimmed = line.trim();

        if self.depth > 0 {
            // Already inside a cfg(test) module: update brace balance.
            self.depth += count_open(line);
            self.depth = self.depth.saturating_sub(count_close(line));
            return true;
        }

        if trimmed.starts_with("#[cfg(")
            && trimmed.contains("test")
            && !trimmed.contains("not(test)")
        {
            self.pending_cfg_test = true;
            return false;
        }

        if self.pending_cfg_test {
            if trimmed.is_empty() || trimmed.starts_with("#[") || trimmed.starts_with("//") {
                // Attributes/blank lines may sit between #[cfg(test)] and the item.
                return false;
            }
            // The attribute applies to the next item. Any braced item (module,
            // function, impl, etc.) creates a test-only region that must be
            // skipped wholesale; otherwise a production audit would count
            // fixture thread/runtime/channel sites inside cfg(test) functions.
            self.pending_cfg_test = false;
            if count_open(line) > count_close(line) {
                self.depth = count_open(line).saturating_sub(count_close(line));
                return true;
            }
            if !trimmed.ends_with(';') {
                // Multi-line item header: keep consuming test-only lines until
                // its opening brace appears.
                self.pending_cfg_test = true;
            }
            // A single `#[cfg(test)]` item (use/fn/const/static). Skip this line.
            return true;
        }

        false
    }
}

fn count_open(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'{').count() as u32
}
fn count_close(s: &str) -> u32 {
    s.bytes().filter(|&b| b == b'}').count() as u32
}

/// A banned-API class and the regex-free matchers describing it.
struct BannedClass {
    name: &'static str,
    /// Substrings; a line matches the class if it contains any of them.
    needles: &'static [&'static str],
    /// Files (repo-relative) where this class is sanctioned.
    allowlist: &'static [&'static str],
}

fn line_matches(line: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| line.contains(n))
}

/// Run the chokepoint scan for one banned class and return offending
/// `path:line: text` strings that are NOT in the allowlist.
fn scan_class(root: &Path, files: &[PathBuf], class: &BannedClass) -> Vec<String> {
    let mut violations = Vec::new();

    for rel in files {
        if is_excluded_file(rel) {
            continue;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let allowed = class.allowlist.iter().any(|entry| {
            entry
                .strip_suffix('/')
                .map_or(rel_str == *entry, |directory| {
                    rel_str.starts_with(directory)
                        && rel_str.as_bytes().get(directory.len()) == Some(&b'/')
                })
        });
        let abs = root.join(rel);
        let content =
            std::fs::read_to_string(&abs).unwrap_or_else(|err| panic!("read {abs:?}: {err}"));
        let mut tracker = CfgTestTracker::new();
        for (idx, raw) in content.lines().enumerate() {
            let in_test = tracker.in_test(raw);
            if allowed {
                continue; // still need to advance the tracker above
            }
            if in_test {
                continue;
            }
            let code = strip_line_comment(raw);
            if line_matches(code, class.needles) {
                violations.push(format!("{}:{}: {}", rel_str, idx + 1, raw.trim()));
            }
        }
    }
    violations
}

// ---------------------------------------------------------------------------
// Allowlists -- built from the CURRENT legitimate uses (green today).
// ---------------------------------------------------------------------------

/// fs: host filesystem access.
///
/// Sanctioned surface: the sidecar host-FS plumbing + VFS-backed runtime, the
/// JS/Python/WASM runtime asset & module loaders, the sidecar bootstrap
/// (stdio/service/state/vm), and runtime support glue. These modules read
/// real host files to seed the VFS, load runtime assets, and bridge guest FS
/// syscalls to the host-dir mount.
const FS_ALLOW: &[&str] = &[
    // sidecar host-FS chokepoint + bootstrap. `host_dir.rs` also contains the
    // universal host-mount confinement primitive (the `confine` module: the
    // single resolve-beneath walk using plain `openat(2)`, fd-anchored, no
    // `openat2`, running identically on Linux, macOS, and gVisor). It replaced
    // the deleted macOS-only `macos_fs.rs` cap-std fallback; see the `confine`
    // module docs for why `openat2` was removed.
    "crates/native-sidecar/src/filesystem.rs",
    "crates/native-sidecar/src/plugins/host_dir.rs",
    "crates/native-sidecar/src/plugins/module_access.rs",
    // agentOS package projection: the sidecar is the host-side TCB that reads a
    // trusted, client-configured package's tar + `agentos-package.json` from the
    // host to build the read-only `/opt/agentos` granular mounts (no extraction,
    // no on-disk symlink farm). Same sanctioned read-only host-source boundary as
    // filesystem.rs/host_dir.rs.
    "crates/native-sidecar/src/package_projection.rs",
    "crates/native-sidecar/src/stdio.rs",
    "crates/native-sidecar/src/state.rs",
    "crates/native-sidecar/src/vm.rs",
    "crates/native-sidecar/src/service.rs",
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/plugins/chunked_local.rs",
    "crates/vfs-store/src/local/file_block_store.rs",
    "crates/vfs-store/src/local/sqlite_metadata_store.rs",
    // Package-format tooling reads and writes caller-selected host artifacts;
    // it never handles guest paths at runtime.
    "crates/vfs/src/package_format/mod.rs",
    "crates/vfs/src/package_format/pack.rs",
    // ACP trace output is an operator-selected host diagnostic sink. The
    // extension is split mechanically across its module root and restore path.
    "crates/agentos-sidecar/src/acp/mod.rs",
    "crates/agentos-sidecar/src/acp/restore.rs",
    // Tar-backed read-only VFS: mmaps the trusted, client-configured package
    // tar from the host and serves member byte ranges without extracting.
    // Same sanctioned read-only host-source boundary as host_dir.rs (the tar is
    // an immutable, content-addressed mount source); reads are SIGBUS-guarded.
    "crates/vfs/src/posix/tar_fs.rs",
    // language-runtime asset / module loaders (read host runtime assets)
    "crates/execution/src/python.rs",
    "crates/execution/src/wasm.rs",
    "crates/execution/src/javascript.rs",
    "crates/execution/src/node_import_cache.rs",
    "crates/execution/src/runtime_support.rs",
    // Host-side V8 diagnostics: module-trace and sync-RPC latency profilers
    // write to an operator-provided file path, and snapshot bootstrap reads the
    // userland bundle from PI_SNAPSHOT_BUNDLE_PATH. Host-only, not guest-reachable.
    "crates/v8-runtime/src/execution.rs",
    "crates/v8-runtime/src/host_call.rs",
    "crates/v8-runtime/src/snapshot.rs",
    // Session-phase perf recorder writes to an operator-provided file path
    // (AGENTOS_V8_SESSION_PHASES_FILE). Host-only diagnostics, same class as
    // execution.rs/host_call.rs above.
    "crates/v8-runtime/src/session.rs",
];

/// net: host network access.
///
/// Sanctioned surface: the kernel DNS resolver plane, the sidecar host-net
/// chokepoint (`execution.rs`, which owns all guest TCP/UDP/Unix sockets), the
/// host-backed storage/agent plugins (which open egress to S3 / Google Drive /
/// the sandbox-agent control plane), the embedded V8 runtime IPC socketpair,
/// and the client transport that talks to the spawned sidecar.
const NET_ALLOW: &[&str] = &[
    // kernel network plane
    "crates/kernel/src/dns.rs",
    // Shared IP classifier only; no host sockets are opened here.
    "crates/kernel/src/network_policy.rs",
    // Shared socket-address formatting only; no host sockets are opened here.
    "crates/native-sidecar-core/src/net.rs",
    "crates/kernel/src/socket_table.rs",
    "crates/kernel/src/kernel.rs",
    // sidecar host-net chokepoint + bootstrap
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/state.rs",
    "crates/native-sidecar/src/vm.rs",
    // Required inherited fd-3 response/control IPC stream; no external egress.
    "crates/native-sidecar/src/stdio.rs",
    // host-backed storage / agent plugins (network egress)
    "crates/native-sidecar/src/plugins/s3_common.rs",
    "crates/vfs-store/src/s3/block_store.rs",
    "crates/vfs-store/src/s3/object_backend.rs",
    "crates/native-sidecar/src/plugins/google_drive.rs",
    "crates/native-sidecar/src/plugins/sandbox_agent.rs",
    // embedded runtime IPC socketpair (not external egress)
    "crates/v8-runtime/src/embedded_runtime.rs",
    "crates/execution/src/v8_host.rs",
    "crates/execution/src/v8_runtime.rs",
    // client spawns + connects to the sidecar helper
    "crates/sidecar-client/src/transport.rs",
    // Authenticated local transport from the sidecar to the owning actor's
    // SQLite UDS endpoint. This is local IPC, not external network egress.
    "crates/actor-uds-client/src/lib.rs",
    // Test-only actor SQLite UDS fixture; it opens local Unix sockets but no
    // external network connection.
    "crates/agentos-sidecar/src/session_store/performance_tests.rs",
];

/// process: OS subprocess creation.
///
/// Sanctioned surface: only the client transport, which spawns agentos's
/// own sidecar helper binary. Guest "process" spawns go through the kernel
/// `CommandDriver` registry and never reach `Command::new`.
const PROCESS_ALLOW: &[&str] = &[
    "crates/sidecar-client/src/transport.rs",
    // V8 snapshot builder re-execs agentos's OWN binary as a helper
    // (SNAPSHOT_HELPER_ENV) so snapshot creation runs in a clean process.
    // Host-side bootstrap only; no guest-controlled input picks the program.
    "crates/v8-runtime/src/snapshot.rs",
];

/// env: process-environment reads.
///
/// Sanctioned surface: the scrubbed/bootstrap configuration readers that look
/// up host configuration (sidecar binary path, node binary path/PATH, codec
/// selection, subprocess re-exec markers, local-endpoint test escape hatch)
/// before a VM exists.
const ENV_ALLOW: &[&str] = &[
    "crates/sidecar-client/src/transport.rs",
    "crates/client/src/sidecar.rs",
    // Operator-selected ACP trace output path.
    "crates/agentos-sidecar/src/acp/restore.rs",
    "crates/agentos-sidecar/src/main.rs",
    "crates/execution/src/host_node.rs",
    // Node import cache reads an operator timeout knob before materializing
    // host-side runtime assets for VM startup.
    "crates/execution/src/node_import_cache.rs",
    // Host-side perf phase diagnostics toggles, read from operator env and not
    // guest-reachable.
    "crates/execution/src/javascript.rs",
    "crates/native-sidecar/src/filesystem.rs",
    "crates/v8-runtime/src/bridge.rs",
    "crates/native-sidecar/src/execution/",
    "crates/native-sidecar/src/plugins/s3_common.rs",
    // Host-process startup log-level knob, read before any VM exists.
    "crates/native-sidecar/src/main.rs",
    // Host-side V8 diagnostics toggles (module-trace + sync-RPC latency
    // profiling + snapshot-bundle path), read at runtime init from operator
    // env. Not guest-reachable.
    "crates/v8-runtime/src/execution.rs",
    "crates/v8-runtime/src/host_call.rs",
    "crates/v8-runtime/src/snapshot.rs",
    // Browser sidecar reads a test-only vm.fetch timeout override (bucket 1:
    // process-wide test/debug knob, native-only); not VM policy.
    // Warm-isolate pool sizing knob (AGENTOS_V8_WARM_ISOLATES), read at
    // executor init from operator env. Not guest-reachable.
    "crates/execution/src/v8_host.rs",
    // Wasm runner mode/cache knobs (AGENTOS_WASM_SNAPSHOT_RUNNER,
    // AGENTOS_WASM_RUNNER_NO_CACHE) + warm-pool sizing, read at executor init
    // from operator env. Not guest-reachable. (wasm.rs is already a sanctioned
    // FS asset-loading boundary above.)
    "crates/execution/src/wasm.rs",
    // Session-phase perf diagnostics toggles (AGENTOS_V8_SESSION_PHASES*),
    // read from operator env. Not guest-reachable.
    "crates/v8-runtime/src/session.rs",
];

fn fs_class() -> BannedClass {
    BannedClass {
        name: "fs",
        needles: &[
            "std::fs",
            "tokio::fs",
            "File::open",
            "File::create",
            "OpenOptions",
            "openat",
        ],
        allowlist: FS_ALLOW,
    }
}

fn net_class() -> BannedClass {
    BannedClass {
        name: "net",
        needles: &[
            "std::net::",
            "tokio::net::",
            "reqwest::",
            "reqwest ",
            "hyper::",
            "TcpStream::",
            "TcpListener::bind",
            "UdpSocket::bind",
            "UnixStream::connect",
            "UnixStream::pair",
            "UnixListener::bind",
            ".to_socket_addrs(",
            "std::os::unix::net",
        ],
        allowlist: NET_ALLOW,
    }
}

fn process_class() -> BannedClass {
    BannedClass {
        name: "process",
        needles: &[
            "std::process::Command",
            "process::Command",
            "tokio::process",
            "Command::new",
            "libc::fork",
            "nix::unistd::fork",
        ],
        allowlist: PROCESS_ALLOW,
    }
}

fn env_class() -> BannedClass {
    BannedClass {
        name: "env",
        needles: &[
            "env::var(",
            "env::var_os(",
            "env::vars(",
            "env::vars_os(",
            "std::env::var",
        ],
        allowlist: ENV_ALLOW,
    }
}

fn assert_green(root: &Path, files: &[PathBuf], class: BannedClass) {
    let violations = scan_class(root, files, &class);
    assert!(
        violations.is_empty(),
        "\n\nChokepoint lint ({}) found {} host-API use(s) OUTSIDE the sanctioned \
allowlist.\nEither route the access through an existing chokepoint, or -- if this \
is a genuinely new sanctioned boundary -- add the file to the `{}` allowlist in \
crates/native-sidecar/tests/architecture_guards.rs with a justifying comment.\n\n{}\n",
        class.name,
        violations.len(),
        match class.name {
            "fs" => "FS_ALLOW",
            "net" => "NET_ALLOW",
            "process" => "PROCESS_ALLOW",
            _ => "ENV_ALLOW",
        },
        violations.join("\n"),
    );
}

#[test]
fn fs_access_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, fs_class());
}

#[test]
fn net_access_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, net_class());
}

#[test]
fn process_spawn_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, process_class());
}

#[test]
fn env_reads_confined_to_chokepoints() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert_green(&root, &files, env_class());
}

/// Sanity: the scan actually sees source files and the allowlisted files exist.
/// Guards against a refactor silently making the lint scan nothing (which would
/// make it vacuously pass).
#[test]
fn lint_scans_real_sources_and_allowlist_paths_exist() {
    let root = repo_root();
    let files = production_source_files(&root);
    assert!(
        files.len() > 30,
        "expected to scan many source files, found {}",
        files.len()
    );

    let mut missing = Vec::new();
    for class in [FS_ALLOW, NET_ALLOW, PROCESS_ALLOW, ENV_ALLOW] {
        for rel in class {
            let path = root.join(rel);
            let exists = if rel.ends_with('/') {
                path.is_dir()
            } else {
                path.is_file()
            };
            if !exists {
                missing.push(rel.to_string());
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "allowlist references files that no longer exist (clean them up): {missing:?}"
    );
}

// ---------------------------------------------------------------------------
// Runtime topology and lower-layer dependency guards.
// ---------------------------------------------------------------------------

fn dependency_keys(manifest: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("read {manifest:?}: {error}"));
    let mut dependencies = BTreeSet::new();
    let mut in_dependencies = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            in_dependencies = line.contains("dependencies");
            continue;
        }
        if !in_dependencies || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line
            .split(['=', ' ', '\t'])
            .next()
            .unwrap_or("")
            .trim_matches('"');
        if !key.is_empty() {
            dependencies.insert(key.to_owned());
        }
    }
    dependencies
}

#[test]
fn generic_runtime_layers_do_not_depend_on_product_or_acp_layers() {
    let root = repo_root();
    let lower_layers = [
        "runtime",
        "kernel",
        "vfs",
        "vfs-store",
        "v8-runtime",
        "execution",
    ];
    let forbidden = [
        "agentos-protocol",
        "agentos-sidecar-core",
        "agentos-sidecar",
        "agentos-client",
        "agentos-actor-plugin",
    ];
    let mut violations = Vec::new();
    for crate_dir in lower_layers {
        let manifest = root.join("crates").join(crate_dir).join("Cargo.toml");
        let dependencies = dependency_keys(&manifest);
        for dependency in forbidden {
            if dependencies.contains(dependency) {
                violations.push(format!("crates/{crate_dir}: {dependency}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "generic runtime layers depend on product/ACP layers:\n{}",
        violations.join("\n")
    );
}

#[test]
fn shared_acp_runtime_has_no_adapter_name_policy() {
    let root = repo_root();
    let production = ["mod.rs", "runtime.rs", "restore.rs", "turn.rs"]
        .into_iter()
        .map(|file| {
            let source =
                std::fs::read_to_string(root.join("crates/agentos-sidecar/src/acp").join(file))
                    .unwrap_or_else(|error| panic!("read native ACP module {file}: {error}"));
            source
                .split("#[cfg(test)]")
                .next()
                .unwrap_or(&source)
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n");
    for adapter_name in [
        "\"claude\"",
        "\"codex\"",
        "\"opencode\"",
        "\"pi\"",
        "\"pi-cli\"",
    ] {
        assert!(
            !production.contains(adapter_name),
            "shared ACP runtime must not branch on adapter name {adapter_name}; put launch compatibility in the AgentOS-owned package launcher"
        );
    }
    assert!(
        production.contains("ACP_APPEND_SYSTEM_PROMPT_ENV"),
        "shared ACP runtime must use the adapter-neutral package-launch contract"
    );
}

#[test]
fn typescript_sdk_does_not_ship_a_competing_in_memory_vfs() {
    let root = repo_root();
    for relative_path in [
        "packages/core/src/runtime-compat.ts",
        "packages/core/src/index.ts",
        "packages/core/src/layers.ts",
        "packages/runtime-core/src/node-runtime.ts",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            !source.contains("createInMemoryFileSystem")
                && !source.contains("class InMemoryFileSystem")
                && !source.contains("createInMemoryLayerStore"),
            "production TypeScript SDK must not implement or export an in-memory VFS: {relative_path}"
        );
    }
    assert!(
        root.join("packages/runtime-core/src/test-runtime.ts")
            .is_file(),
        "the explicit test-only VFS callback fixture must remain available"
    );
    let low_level_runtime =
        std::fs::read_to_string(root.join("packages/runtime-core/src/node-runtime.ts"))
            .expect("read low-level Node runtime");
    assert!(
        low_level_runtime.contains("filesystem: VirtualFileSystem")
            && low_level_runtime.contains("const filesystem = options.filesystem"),
        "the low-level compatibility runtime must require a caller-owned filesystem instead of creating a TypeScript default"
    );
}

#[test]
fn rust_client_transport_routes_live_events_without_history() {
    let root = repo_root();
    let source = std::fs::read_to_string(root.join("crates/sidecar-client/src/transport.rs"))
        .expect("read Rust sidecar transport");
    for obsolete in [
        "WireEventLog",
        "route_sequence",
        "global_sequence",
        "provisional_process",
    ] {
        assert!(
            !source.contains(obsolete),
            "client transport must not retain replay/history state ({obsolete})"
        );
    }
    assert!(
        source.contains("broadcast::channel(EVENT_CHANNEL_CAPACITY)"),
        "client transport must retain only bounded live event fan-out"
    );
}

fn native_reactor_source_files(root: &Path) -> Vec<PathBuf> {
    production_source_files(root)
        .into_iter()
        .filter(|path| {
            let path = path.to_string_lossy();
            [
                "crates/bridge/",
                "crates/execution/",
                "crates/kernel/",
                "crates/native-sidecar/",
                "crates/native-sidecar-core/",
                "crates/runtime/",
                "crates/sidecar-protocol/",
                "crates/v8-runtime/",
                "crates/vfs/",
                "crates/vfs-store/",
                "crates/vm-config/",
            ]
            .iter()
            .any(|prefix| path.starts_with(prefix))
        })
        .collect()
}

fn native_execution_source_files(root: &Path) -> Vec<PathBuf> {
    production_source_files(root)
        .into_iter()
        .filter(|path| path.starts_with("crates/native-sidecar/src/execution"))
        .collect()
}

fn native_execution_source(root: &Path) -> String {
    native_execution_source_files(root)
        .into_iter()
        .map(|path| {
            std::fs::read_to_string(root.join(&path))
                .unwrap_or_else(|error| panic!("read {path:?}: {error}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_execution_is_split_by_domain() {
    let root = repo_root();
    let expected = [
        "crates/native-sidecar/src/execution/mod.rs",
        "crates/native-sidecar/src/execution/coordinator.rs",
        "crates/native-sidecar/src/execution/launch.rs",
        "crates/native-sidecar/src/execution/process.rs",
        "crates/native-sidecar/src/execution/process_events.rs",
        "crates/native-sidecar/src/execution/child_process.rs",
        "crates/native-sidecar/src/execution/signals.rs",
        "crates/native-sidecar/src/execution/stdio.rs",
        "crates/native-sidecar/src/execution/network/mod.rs",
        "crates/native-sidecar/src/execution/network/tcp.rs",
        "crates/native-sidecar/src/execution/network/unix.rs",
        "crates/native-sidecar/src/execution/network/udp.rs",
        "crates/native-sidecar/src/execution/network/tls.rs",
        "crates/native-sidecar/src/execution/network/http2.rs",
        "crates/native-sidecar/src/execution/network/dns.rs",
        "crates/native-sidecar/src/execution/javascript/mod.rs",
        "crates/native-sidecar/src/execution/javascript/rpc.rs",
        "crates/native-sidecar/src/execution/javascript/crypto.rs",
        "crates/native-sidecar/src/execution/javascript/sqlite.rs",
        "crates/native-sidecar/src/execution/javascript/http.rs",
        "crates/native-sidecar/src/execution/python/mod.rs",
        "crates/native-sidecar/src/execution/python/rpc.rs",
        "crates/native-sidecar/src/execution/python/sockets.rs",
        "crates/native-sidecar/src/execution/python/subprocess.rs",
    ];

    for path in expected {
        assert!(root.join(path).is_file(), "missing execution module {path}");
    }
    assert!(
        !root.join("crates/native-sidecar/src/execution.rs").exists(),
        "the monolithic execution.rs must not be restored"
    );
}

fn production_matches(root: &Path, files: &[PathBuf], needles: &[&str]) -> Vec<String> {
    let mut matches = Vec::new();
    for rel in files {
        if is_excluded_file(rel) {
            continue;
        }
        let content = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|error| panic!("read {rel:?}: {error}"));
        let mut tracker = CfgTestTracker::new();
        for (index, raw) in content.lines().enumerate() {
            if tracker.in_test(raw) {
                continue;
            }
            let code = strip_line_comment(raw);
            if needles.iter().any(|needle| code.contains(needle)) {
                matches.push(format!("{}:{}: {}", rel.display(), index + 1, raw.trim()));
            }
        }
    }
    matches
}

#[test]
fn native_sidecar_dependency_closure_has_one_tokio_runtime_builder() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let builders = production_matches(
        &root,
        &files,
        &[
            "Builder::new_multi_thread()",
            "Builder::new_current_thread()",
        ],
    );
    assert_eq!(
        builders.len(),
        1,
        "expected exactly one production Tokio runtime builder:\n{}",
        builders.join("\n")
    );
    assert!(
        builders[0].starts_with("crates/runtime/src/lib.rs:"),
        "the one runtime builder must be process-owned: {}",
        builders[0]
    );
}

#[test]
fn production_subsystems_use_injected_runtime_contexts() {
    let root = repo_root();
    let files = native_reactor_source_files(&root)
        .into_iter()
        .filter(|path| path != Path::new("crates/runtime/src/lib.rs"))
        .collect::<Vec<_>>();
    let violations = production_matches(&root, &files, &["SidecarRuntime::process_context("]);
    assert!(
        violations.is_empty(),
        "production subsystems must receive an injected VM/process RuntimeContext:\n{}",
        violations.join("\n")
    );
}

#[test]
fn native_reactor_never_uses_tokios_elastic_blocking_pool() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "tokio::task::spawn_blocking",
            "spawn_blocking(",
            "block_in_place(",
        ],
    );
    assert!(
        violations.is_empty(),
        "blocking work must use the fixed, byte-admitted sidecar executor:\n{}",
        violations.join("\n")
    );
}

#[test]
fn native_execution_dispatch_never_blocks_on_completion_or_polling() {
    let root = repo_root();
    let files = native_execution_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "recv_timeout(",
            "mpsc::sync_channel(",
            ".wait_timeout(",
            ".poll_event_blocking(",
            "thread::sleep(",
            "std::thread::sleep(",
        ],
    );
    assert!(
        violations.is_empty(),
        "native dispatch must defer async completions and wait on reactor readiness; it may not block or poll:\n{}",
        violations.join("\n")
    );
}

#[test]
fn top_level_python_start_uses_the_async_runtime_adapter() {
    let path = repo_root().join("crates/native-sidecar/src/execution/launch.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    assert!(
        source.contains(
            ".python_engine\n                    .start_execution_with_runtime_async("
        ),
        "top-level Python startup must await cache materialization and prewarm instead of blocking a Tokio worker"
    );
    assert!(
        source.contains(".bundled_pyodide_dist_path_for_vm_async(&vm_id, &vm.runtime_context)"),
        "top-level Pyodide cache materialization must not run synchronously before the async Python start"
    );
}

#[test]
fn nested_child_start_never_blocks_the_shared_runtime_worker() {
    let source = native_execution_source(&repo_root());

    assert!(
        source.contains("pub(crate) async fn spawn_javascript_child_process("),
        "root child startup must be an async sidecar dispatch path"
    );
    assert!(
        source.contains("async fn spawn_descendant_javascript_child_process("),
        "descendant child startup must be an async sidecar dispatch path"
    );
    assert!(
        source
            .matches(".start_execution_with_runtime_async(")
            .count()
            >= 6,
        "top-level plus root/descendant Python and WASM startup must use async runtime adapters"
    );
    assert!(
        !source.contains(".start_execution_with_runtime(\n                            StartPythonExecutionRequest")
            && !source.contains(
                ".start_execution_with_runtime(\n                            StartWasmExecutionRequest",
            ),
        "Python/WASM child startup must not synchronously prewarm on a Tokio worker"
    );
}

#[test]
fn reactor_readiness_never_uses_the_ordinary_stream_event_lane() {
    let root = repo_root();
    let mut files = native_execution_source_files(&root);
    files.extend([
        PathBuf::from("crates/native-sidecar/src/vm.rs"),
        PathBuf::from("crates/execution/src/javascript.rs"),
    ]);
    let violations = production_matches(
        &root,
        &files,
        &[
            "send_stream_event(\"net_socket\"",
            "send_stream_event(\"signal\"",
            "send_javascript_stream_event(\"signal\"",
            "send_stream_event(\"timer\"",
        ],
    );
    assert!(
        violations.is_empty(),
        "socket, protocol, signal, and timer readiness must update durable broker state and publish one coalesced wake; it may not enqueue ordinary per-event messages:\n{}",
        violations.join("\n")
    );
}

#[test]
fn javascript_tcp_receive_path_is_event_driven() {
    let root = repo_root();
    for (relative_path, legacy_poll_markers) in [
        (
            "packages/build-tools/bridge-src/builtins/net.ts",
            &[
                "_netSocketPollRaw",
                "NET_BRIDGE_POLL_DELAY_MS",
                "netBridgePollDelay",
                "setPollDelayMs",
                "scheduleSocketPoll",
                "scheduleServerPoll",
                "net.poll",
                "net.server_poll",
            ][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/network.ts",
            &["NET_BRIDGE_POLL_DELAY_MS", "netBridgePollDelay"][..],
        ),
        (
            "packages/runtime-benchmarks/src/focused/net-tcp-event-floor.bench.ts",
            &["net-poll-delay-ms", "setPollDelayMs", "pollDelayMs"][..],
        ),
        (
            "crates/execution/src/node_import_cache.rs",
            &[
                "NODE_EXECUTION_RUNNER_SOURCE",
                "root_dir.join(\"runner.mjs\")",
                "createRpcBackedNetModule",
                "scheduleSocketPoll",
                "scheduleServerPoll",
                "net.poll",
                "net.server_poll",
            ][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for legacy_poll_marker in legacy_poll_markers {
            assert!(
                !source.contains(legacy_poll_marker),
                "JavaScript TCP sockets and listeners must consume coalesced sidecar readiness, not a recurring synchronous poll bridge ({legacy_poll_marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn native_reactor_has_no_unbounded_channels_or_per_io_thread_names() {
    let root = repo_root();
    let files = native_reactor_source_files(&root);
    let violations = production_matches(
        &root,
        &files,
        &[
            "unbounded_channel",
            "crossbeam_channel::unbounded",
            "tcp-socket-reader",
            "unix-socket-reader",
            "kernel-wait-rpc",
            "signal-delivery-thread",
            "http2-runtime-thread",
            "EVENT_PUMP_INTERVAL",
            "remaining.min(Duration::from_millis(10))",
        ],
    );
    assert!(
        violations.is_empty(),
        "native reactor contains forbidden unbounded/thread-per-I/O patterns:\n{}",
        violations.join("\n")
    );
}

#[test]
fn native_reactor_tasks_enter_through_task_supervision() {
    let root = repo_root();
    let files = native_reactor_source_files(&root)
        .into_iter()
        // This is the sole implementation of the supervised spawn API. Its
        // Handle::spawn calls run only after TaskSupervisor admission.
        .filter(|path| path != Path::new("crates/runtime/src/lib.rs"))
        .collect::<Vec<_>>();
    let violations = production_matches(
        &root,
        &files,
        &[
            "tokio::spawn(",
            "tokio::task::spawn(",
            "Handle::current().spawn(",
            ".handle().spawn(",
            ".handle.spawn(",
        ],
    );
    assert!(
        violations.is_empty(),
        "native reactor tasks must enter through RuntimeContext's supervised spawn API:\n{}",
        violations.join("\n")
    );
}

#[test]
fn v8_platform_worker_pool_has_a_reviewed_fixed_bound() {
    let root = repo_root();
    let path = root.join("crates/v8-runtime/src/isolate.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    assert!(
        source.contains("const V8_PLATFORM_WORKER_THREADS: u32 = 4;")
            && source.contains("v8::new_default_platform(V8_PLATFORM_WORKER_THREADS, false)"),
        "V8's internal platform workers must use the reviewed fixed four-thread bound"
    );
}

#[test]
fn production_threads_match_the_reviewed_topology_manifest() {
    const MANIFEST: &[(&str, &str)] = &[
        ("blocking-executor-worker", "crates/runtime/src/lib.rs"),
        (
            "constant-v8-platform-owner",
            "crates/v8-runtime/src/isolate.rs",
        ),
        (
            "embedded-v8-dispatch",
            "crates/v8-runtime/src/embedded_runtime.rs",
        ),
        (
            "embedded-v8-writer",
            "crates/v8-runtime/src/embedded_runtime.rs",
        ),
        ("bounded-v8-warm-worker", "crates/v8-runtime/src/session.rs"),
        (
            "admitted-v8-session-executor",
            "crates/v8-runtime/src/session.rs",
        ),
        (
            "serialized-v8-maintenance",
            "crates/execution/src/v8_host.rs",
        ),
        (
            "constant-stdio-writer",
            "crates/native-sidecar/src/stdio.rs",
        ),
        (
            "constant-stdio-reader",
            "crates/native-sidecar/src/stdio.rs",
        ),
    ];

    let root = repo_root();
    let mut observed = BTreeSet::new();
    let mut unmarked = Vec::new();
    // This census covers every production crate, not only the reactor's
    // dependency closure. ACP/session or client-side support code runs in the
    // same sidecar process and may not introduce an unreviewed OS thread either.
    for rel in production_source_files(&root) {
        if is_excluded_file(&rel) {
            continue;
        }
        let content = std::fs::read_to_string(root.join(&rel))
            .unwrap_or_else(|error| panic!("read {rel:?}: {error}"));
        let lines = content.lines().collect::<Vec<_>>();
        let mut tracker = CfgTestTracker::new();
        for (index, raw) in lines.iter().enumerate() {
            if tracker.in_test(raw) {
                continue;
            }
            let code = strip_line_comment(raw);
            if ![
                "thread::spawn(",
                "std::thread::spawn(",
                "thread::Builder::new()",
                "std::thread::Builder::new()",
            ]
            .iter()
            .any(|needle| code.contains(needle))
            {
                continue;
            }
            let marker = lines[index.saturating_sub(3)..index]
                .iter()
                .rev()
                .find_map(|line| line.split("AGENTOS_THREAD_SITE: ").nth(1))
                .map(str::trim);
            match marker {
                Some(marker) => {
                    observed.insert((marker.to_owned(), rel.to_string_lossy().replace('\\', "/")));
                }
                None => unmarked.push(format!("{}:{}: {}", rel.display(), index + 1, raw.trim())),
            }
        }
    }

    assert!(
        unmarked.is_empty(),
        "production OS thread sites must carry a reviewed AGENTOS_THREAD_SITE marker:\n{}",
        unmarked.join("\n")
    );
    let expected = MANIFEST
        .iter()
        .map(|(marker, path)| ((*marker).to_owned(), (*path).to_owned()))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        observed, expected,
        "production thread topology changed without updating the reviewed manifest"
    );
}

#[test]
fn javascript_dgram_receive_path_is_event_driven() {
    let path = repo_root().join("packages/build-tools/bridge-src/builtins/dgram.ts");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for legacy_poll_marker in ["_receivePollTimer", "NET_BRIDGE_POLL_DELAY_MS"] {
        assert!(
            !source.contains(legacy_poll_marker),
            "JavaScript dgram receive must wait for coalesced sidecar readiness, not recurring polling ({legacy_poll_marker})"
        );
    }
}

#[test]
fn javascript_http2_receive_path_is_event_driven() {
    let path = repo_root().join("packages/build-tools/bridge-src/builtins/http2.ts");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for legacy_poll_marker in ["fallbackTimer", "setTimeout(tick"] {
        assert!(
            !source.contains(legacy_poll_marker),
            "JavaScript HTTP/2 receive must wait for coalesced sidecar readiness, not recurring polling ({legacy_poll_marker})"
        );
    }
}

#[test]
fn protocol_and_abort_delivery_have_no_recurring_poll_timer() {
    let root = repo_root();
    for (relative_path, forbidden) in [
        (
            "crates/agentos-sidecar/src/acp/runtime.rs",
            &["ACP_JSON_RPC_POLL_INTERVAL", "remaining.min(ACP_"][..],
        ),
        (
            "crates/native-sidecar/src/stdio.rs",
            &["write_rx.recv_timeout(Duration::from_millis(5))"][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/http.ts",
            &["_startAbortSignalPoll", "_signalPollTimer"][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/fs.ts",
            &[
                "setTimeout(attemptKernelStdinRead",
                "setTimeout(attemptRead",
                "_kernelStdinRead.apply(void 0, [length, 100]",
            ][..],
        ),
        (
            "packages/build-tools/bridge-src/builtins/stdin.ts",
            &["_kernelStdinRead.apply(void 0, [65536, 100]"][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "protocol/abort delivery must wait on a direct event notification, not recurring polling ({marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn standalone_wasm_wait_has_no_recurring_adapter_poll() {
    let path = repo_root().join("crates/execution/src/wasm.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
    for marker in [
        "self.poll_event_blocking(Duration::from_millis(50))",
        "Sample elapsed budget each poll",
    ] {
        assert!(
            !source.contains(marker),
            "standalone WASM waits must block on readiness with one deadline-aware wait, not a recurring adapter poll ({marker})"
        );
    }
    assert!(
        source.contains("fn wait_event_blocking("),
        "standalone WASM wait must retain its direct readiness/deadline wait helper"
    );
}

#[test]
fn browser_sources_are_retained_but_disabled_from_native_build_and_publish_gates() {
    let root = repo_root();
    for relative_path in [
        "crates/agentos-sidecar-core/src",
        "crates/agentos-sidecar-browser/src",
        "crates/native-sidecar-browser/src",
        "packages/browser/src",
        "packages/runtime-browser/src",
    ] {
        assert!(
            root.join(relative_path).is_dir(),
            "browser migration source must remain retained at {relative_path}"
        );
    }

    for relative_path in [
        "crates/agentos-sidecar-browser/src/lib.rs",
        "crates/native-sidecar-browser/src/lib.rs",
        "packages/browser/src/index.ts",
        "packages/runtime-browser/src/index.ts",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            source.contains("AGENTOS_BROWSER_SUPPORT_DISABLED"),
            "browser public entrypoint must remain disabled: {relative_path}"
        );
        assert!(
            source.lines().any(|line| line.trim() == "/*"),
            "browser public entrypoint source must remain commented out: {relative_path}"
        );
        if relative_path.ends_with(".rs") {
            assert!(
                source.trim_end().ends_with("*/"),
                "disabled Rust browser entrypoint must contain no active items after its retained source: {relative_path}"
            );
        } else {
            assert!(
                source.trim_end().ends_with("export {};"),
                "disabled TypeScript browser entrypoint must expose only an empty module: {relative_path}"
            );
        }
    }

    let workspace =
        std::fs::read_to_string(root.join("Cargo.toml")).expect("read workspace Cargo.toml");
    assert!(
        workspace.contains("exclude = [\"software\", \"crates/agentos-sidecar-core\"]"),
        "the obsolete browser-only ACP state machine must remain outside the native workspace"
    );
    let obsolete_core =
        std::fs::read_to_string(root.join("crates/agentos-sidecar-core/Cargo.toml"))
            .expect("read obsolete browser-only ACP core manifest");
    assert!(
        obsolete_core.contains("publish = false"),
        "the obsolete browser-only ACP state machine must not be publishable"
    );
    let default_members = workspace
        .split("default-members = [")
        .nth(1)
        .and_then(|tail| tail.split(']').next())
        .expect("workspace must declare default-members while browser is disabled");
    for browser_crate in [
        "crates/agentos-sidecar-browser",
        "crates/native-sidecar-browser",
    ] {
        assert!(
            workspace.contains(&format!("\"{browser_crate}\"")),
            "retained browser crate must remain a workspace member: {browser_crate}"
        );
        assert!(
            !default_members.contains(browser_crate),
            "disabled browser crate entered Cargo default-members: {browser_crate}"
        );
        let manifest = std::fs::read_to_string(root.join(browser_crate).join("Cargo.toml"))
            .unwrap_or_else(|error| panic!("read {browser_crate}/Cargo.toml: {error}"));
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "publish = false"),
            "disabled browser crate must not be publishable: {browser_crate}"
        );
    }

    for browser_package in ["packages/browser", "packages/runtime-browser"] {
        let manifest = std::fs::read_to_string(root.join(browser_package).join("package.json"))
            .unwrap_or_else(|error| panic!("read {browser_package}/package.json: {error}"));
        assert!(
            manifest.contains("\"private\": true"),
            "disabled browser package must remain private: {browser_package}"
        );
    }

    let publish_discovery =
        std::fs::read_to_string(root.join("scripts/publish/src/lib/packages.ts"))
            .expect("read npm publish discovery");
    for package in [
        "@rivet-dev/agentos-browser",
        "@rivet-dev/agentos-runtime-browser",
    ] {
        assert!(
            publish_discovery.contains(&format!("\"{package}\"")),
            "disabled browser package must remain explicitly denied by publish discovery: {package}"
        );
    }

    for relative_path in [
        "package.json",
        ".github/workflows/ci.yml",
        ".github/workflows/publish.yaml",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        for package in [
            "!@rivet-dev/agentos-browser",
            "!@rivet-dev/agentos-runtime-browser",
        ] {
            assert!(
                source.contains(package),
                "{relative_path} must explicitly filter disabled package {package}"
            );
        }
    }

    for relative_path in [
        ".github/workflows/ci.yml",
        ".github/workflows/ci-nightly.yml",
        "scripts/ci.sh",
    ] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        for browser_crate in ["agentos-sidecar-browser", "agentos-native-sidecar-browser"] {
            assert!(
                source.contains(&format!("--exclude {browser_crate}")),
                "{relative_path} must exclude disabled Rust crate {browser_crate}"
            );
        }
    }

    let mirror_generator =
        std::fs::read_to_string(root.join("scripts/generate-agentos-mirror.mjs"))
            .expect("read compatibility mirror generator");
    assert!(
        mirror_generator.contains("browserShim ? { private: true } : {}")
            && mirror_generator.contains("browserShim ? \"publish = false\" : \"\""),
        "generated browser compatibility shims must remain private and unpublishable"
    );
    for relative_path in [".github/workflows/ci.yml", "scripts/ci.sh"] {
        let source = std::fs::read_to_string(root.join(relative_path))
            .unwrap_or_else(|error| panic!("read {relative_path}: {error}"));
        assert!(
            source.contains("node --test scripts/generate-agentos-mirror.test.mjs"),
            "{relative_path} must enforce compatibility-mirror reproducibility"
        );
    }
}

#[test]
fn nightly_runs_explicit_churn_and_multi_vm_soak_gates() {
    let nightly = std::fs::read_to_string(repo_root().join(".github/workflows/ci-nightly.yml"))
        .expect("read nightly workflow");
    for test_name in [
        "multi_vm_generation_soak_has_no_accounting_or_scheduler_drift",
        "multi_vm_protocol_faults_reconcile_shared_runtime_soak",
    ] {
        assert!(
            nightly.contains(test_name),
            "nightly workflow must invoke ignored closure gate {test_name}"
        );
    }
    assert!(
        nightly.matches("--ignored").count() >= 2,
        "nightly workflow must explicitly opt into both expensive closure gates"
    );
}

#[test]
fn javascript_child_process_receive_path_is_event_driven() {
    let root = repo_root();
    for (relative_path, legacy_poll_markers) in [
        (
            "packages/build-tools/bridge-src/builtins/child-process.ts",
            &[
                "_childProcessPoll",
                "scheduleChildProcessPoll",
                "pumpDetachedChildBootstrap",
            ][..],
        ),
        (
            "crates/execution/src/node_import_cache.rs",
            &["scheduleSyntheticChildPoll", "child_process.poll"][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for legacy_poll_marker in legacy_poll_markers {
            assert!(
                !source.contains(legacy_poll_marker),
                "JavaScript child_process output and exit must arrive through bounded/coalesced sidecar events, not a recurring synchronous poll bridge ({legacy_poll_marker}) in {relative_path}"
            );
        }
    }
}

#[test]
fn reactor_completion_paths_do_not_silently_drop_settlement() {
    let root = repo_root();
    let native_execution = native_execution_source(&root);
    for marker in ["let _ = respond_to.send", "let _ = pending.respond_to.send"] {
        assert!(
            !native_execution.contains(marker),
            "reactor completion/control settlement must classify stale/coalesced delivery or log it; found {marker:?} in native execution modules"
        );
    }
    for (relative_path, forbidden) in [
        (
            "crates/v8-runtime/src/session.rs",
            &[
                "limits.javascript.sessionCommandQueue",
                "runtime.protocol.maxEgressFrames",
                "let _ = entry.shutdown_tx.try_send",
            ][..],
        ),
        (
            "crates/execution/src/javascript.rs",
            &[
                "let _ = v8_session.send_bridge_response",
                "let _ = self.v8_session.send_stream_event",
            ][..],
        ),
    ] {
        let path = root.join(relative_path);
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {path:?}: {error}"));
        for marker in forbidden {
            assert!(
                !source.contains(marker),
                "reactor completion/control settlement must classify stale/coalesced delivery or log it; found {marker:?} in {relative_path}"
            );
        }
    }
}

#[test]
fn structured_audit_delivery_failures_have_a_non_recursive_stderr_fallback() {
    let root = repo_root();
    let service_source = std::fs::read_to_string(root.join("crates/native-sidecar/src/service.rs"))
        .expect("read native-sidecar service source");
    assert!(
        !service_source.contains("let _ = emit_structured_event("),
        "structured audit failures must not be silently discarded in service.rs"
    );
    assert!(
        !native_execution_source(&root).contains("let _ = emit_structured_event("),
        "structured audit failures must not be silently discarded in native execution modules"
    );
    let service = std::fs::read_to_string(root.join("crates/native-sidecar/src/service.rs"))
        .expect("read native-sidecar service source");
    let fallback = service
        .split("fn emit_structured_event_or_stderr")
        .nth(1)
        .and_then(|tail| tail.split("pub(crate) fn structured_event_frame").next())
        .expect("locate structured-event stderr fallback");
    assert!(fallback.contains("eprintln!"));
    assert!(fallback.contains("ERR_AGENTOS_STRUCTURED_EVENT"));
    assert!(
        !fallback.contains("emit_log"),
        "telemetry failure fallback must not recurse through bridge telemetry"
    );
}

#[test]
fn python_native_tcp_connect_is_deferred_through_the_shared_runtime() {
    let source = std::fs::read_to_string(
        repo_root().join("crates/native-sidecar/src/execution/python/sockets.rs"),
    )
    .expect("read native-sidecar Python sockets source");
    let socket_connect_arm = source
        .split("PythonVfsRpcMethod::SocketConnect =>")
        .nth(1)
        .and_then(|tail| tail.split("PythonVfsRpcMethod::SocketSend =>").next())
        .expect("locate Python SocketConnect arm");
    assert!(
        socket_connect_arm.contains("defer_python_native_tcp_connect"),
        "Python external TCP connect must leave the dispatcher as deferred shared-runtime work"
    );
    assert!(
        socket_connect_arm.contains("connect_kernel_loopback"),
        "VM-local kernel connect may remain immediate only through its explicit nonblocking path"
    );
    assert!(
        !socket_connect_arm.contains("ActiveTcpSocket::connect("),
        "Python SocketConnect must not reach the synchronous native TCP constructor"
    );

    let deferred_connect = source
        .split("fn defer_python_native_tcp_connect")
        .nth(1)
        .and_then(|tail| tail.split("fn python_socket_async_context").next())
        .expect("locate deferred Python TCP connect helper");
    for required in [
        "tokio::net::TcpStream::connect",
        "ProcessEventEnvelope",
        "PythonSocketConnectCompletion",
    ] {
        assert!(
            deferred_connect.contains(required),
            "deferred Python TCP connect is missing {required}"
        );
    }
    assert!(
        !deferred_connect.contains("connect_timeout"),
        "deferred Python TCP connect must not call a blocking std socket API"
    );
}

#[test]
fn native_udp_has_one_descriptor_owner_and_no_readiness_clone() {
    let execution = std::fs::read_to_string(
        repo_root().join("crates/native-sidecar/src/execution/network/udp.rs"),
    )
    .expect("read native-sidecar UDP source");
    let state = std::fs::read_to_string(repo_root().join("crates/native-sidecar/src/state.rs"))
        .expect("read native-sidecar state source");
    let owner_task = execution
        .split("struct NativeUdpOwnerTask")
        .nth(1)
        .and_then(|tail| tail.split("async fn run_native_udp_owner").next())
        .expect("locate native UDP task ownership record");
    for required in [
        "socket: tokio::net::UdpSocket",
        "commands: TokioReceiver<NativeUdpCommand>",
        "registration: NativeUdpOwnerRegistration",
    ] {
        assert!(
            owner_task.contains(required),
            "UDP task ownership record is missing {required}"
        );
    }
    let owner = execution
        .split("async fn run_native_udp_owner")
        .nth(1)
        .and_then(|tail| tail.split("fn spawn_native_udp_owner").next())
        .expect("locate native UDP owner task");

    for required in [
        "receive_queue",
        "reserve_udp_receive_buffer",
        "resources.capacity_changed()",
        "socket.try_recv_from",
        "limits.datagram_quantum.min(limits.operation_quantum)",
        "tokio::task::yield_now().await",
    ] {
        assert!(owner.contains(required), "UDP owner is missing {required}");
    }
    let spawn = execution
        .split("fn spawn_native_udp_owner")
        .nth(1)
        .and_then(|tail| tail.split("impl ActiveUdpSocket").next())
        .expect("locate native UDP owner registration");
    for required in [
        "tokio::net::UdpSocket::from_std(socket)",
        "registration.limits.max_handle_commands.max(1)",
        "tokio_channel(capacity)",
        "TaskClass::Udp",
    ] {
        assert!(
            spawn.contains(required),
            "UDP owner spawn is missing {required}"
        );
    }
    assert!(
        execution.contains("if !wake_pending.swap(true, Ordering::AcqRel)")
            && execution.contains("push_socket_event(event_pusher, event)"),
        "native UDP readiness must coalesce to one pending cross-boundary wake"
    );
    let udp_impl = execution
        .split("impl ActiveUdpSocket")
        .nth(1)
        .expect("locate ActiveUdpSocket implementation");
    assert!(
        !execution.contains("spawn_native_udp_readiness") && !udp_impl.contains("try_clone()"),
        "native UDP must not split readiness and I/O across descriptor clones"
    );
    let active_udp = state
        .split("pub(crate) struct ActiveUdpSocket")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "// ---------------------------------------------------------------------------",
            )
            .next()
        })
        .expect("locate ActiveUdpSocket");
    assert!(
        active_udp.contains("native_commands: Option<TokioSender<NativeUdpCommand>>")
            && !active_udp.contains("UdpSocket"),
        "the process registry must retain only the owner mailbox, never a native descriptor"
    );

    let connect = udp_impl
        .split("fn connect<B>")
        .nth(1)
        .and_then(|tail| tail.split("fn disconnect").next())
        .expect("locate UDP connect implementation");
    let kernel_branch = connect
        .split("if use_kernel_loopback")
        .nth(1)
        .and_then(|tail| tail.split("self.submit_native_value_command").next())
        .expect("locate VM-local UDP connect branch");
    assert!(
        kernel_branch.contains("socket_connect_udp_loopback")
            && kernel_branch.contains("kernel_connected_remote_addr")
            && kernel_branch.contains("ActiveUdpValueResult::Immediate")
            && !kernel_branch.contains("ensure_native_owner"),
        "VM-local connected UDP must remain taskless and must not activate the native owner"
    );

    let kernel = std::fs::read_to_string(repo_root().join("crates/kernel/src/kernel.rs"))
        .expect("read kernel source");
    let kernel_connect = kernel
        .split("pub fn socket_connect_udp_loopback")
        .nth(1)
        .and_then(|tail| tail.split("pub fn socket_disconnect_udp").next())
        .expect("locate kernel UDP connect implementation");
    assert!(
        kernel_connect.contains("connect_bound_udp_socket")
            && !kernel_connect.contains("tokio::")
            && !kernel_connect.contains("spawn"),
        "kernel UDP connect must be table state only, with no task or runtime"
    );
}
