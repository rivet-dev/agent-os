mod support;

use agentos_native_sidecar::wire::{
    ConfigureVmRequest, GuestRuntimeKind, MountDescriptor, MountPluginDescriptor, RequestPayload,
    WasmPermissionTier,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    create_vm_wire_with_metadata, execute_wire, new_sidecar, open_session_wire, temp_dir,
    wire_request, wire_vm, write_fixture,
};

const ALLOWED_NODE_BUILTINS: &[&str] = &[
    "buffer",
    "child_process",
    "console",
    "constants",
    "events",
    "fs",
    "path",
    "stream",
    "string_decoder",
    "timers",
    "url",
    "util",
];

fn strip_benign_child_pid_warnings(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|line| !line.contains("WARN") || !line.contains("could not retrieve pid"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn registry_command_root() -> PathBuf {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root");
    let copied = repo_root.join("software/coreutils/wasm");
    if copied.exists() {
        return copied;
    }

    let fallback = repo_root.join("toolchain/target/wasm32-wasip1/release/commands");
    if fallback.exists() {
        return fallback;
    }

    panic!(
        "registry WASM commands are required for posix path repro tests: expected {} or {}",
        copied.display(),
        fallback.display()
    );
}

fn configure_mounts(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    include_registry_commands: bool,
    mut mounts: Vec<MountDescriptor>,
) {
    if include_registry_commands {
        let command_root = registry_command_root();
        mounts.insert(
            0,
            MountDescriptor {
                guest_path: String::from("/__agentos/commands/0"),
                guest_source: String::from("host_dir"),
                guest_fstype: String::from("host_dir"),
                read_only: true,
                plugin: MountPluginDescriptor {
                    id: String::from("host_dir"),
                    config: serde_json::to_string(&json!({
                        "hostPath": command_root,
                        "readOnly": true,
                    }))
                    .expect("serialize registry command mount config"),
                },
            },
        );
    }

    sidecar
        .dispatch_wire_blocking(wire_request(
            10,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ConfigureVmRequest(ConfigureVmRequest {
                mounts,
                software: Vec::new(),
                permissions: None,
                module_access_cwd: None,
                instructions: Vec::new(),
                projected_modules: Vec::new(),
                command_permissions: HashMap::from([
                    (String::from("bash"), WasmPermissionTier::Full),
                    (String::from("spawn-test-host"), WasmPermissionTier::Full),
                ]),
                loopback_exempt_ports: Vec::new(),
                packages: Vec::new(),
                packages_mount_at: String::new(),
                bootstrap_commands: Vec::new(),
                binding_shim_commands: Vec::new(),
            }),
        ))
        .expect("configure registry command mount");
}

fn run_host_probe(cwd: &Path, entrypoint: &Path) -> Value {
    let output = Command::new("node")
        .arg(entrypoint)
        .current_dir(cwd)
        .output()
        .expect("run host node probe");

    assert!(
        output.status.success(),
        "host probe failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("parse host probe JSON")
}

fn run_guest_probe_process(
    case_name: &str,
    cwd: &Path,
    entrypoint: &Path,
    mount_registry_commands: bool,
    extra_metadata: HashMap<String, String>,
    extra_mounts: Vec<MountDescriptor>,
) -> (String, String, i32) {
    let mut sidecar = new_sidecar(case_name);
    let connection_id = authenticate_wire(&mut sidecar, &format!("conn-{case_name}"));
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let allowed_builtins =
        serde_json::to_string(ALLOWED_NODE_BUILTINS).expect("serialize builtin allowlist");
    let mut metadata = HashMap::from([(
        String::from("env.AGENTOS_ALLOWED_NODE_BUILTINS"),
        allowed_builtins,
    )]);
    metadata.extend(extra_metadata);
    let (vm_id, _) = create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::JavaScript,
        cwd,
        metadata,
    );

    if mount_registry_commands || !extra_mounts.is_empty() {
        configure_mounts(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            mount_registry_commands,
            extra_mounts,
        );
    }

    execute_wire(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("proc-{case_name}"),
        GuestRuntimeKind::JavaScript,
        entrypoint,
        Vec::new(),
    );

    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &format!("proc-{case_name}"),
        Duration::from_secs(30),
    );

    (stdout, stderr, exit_code)
}

fn run_guest_probe(
    case_name: &str,
    cwd: &Path,
    entrypoint: &Path,
    mount_registry_commands: bool,
    extra_metadata: HashMap<String, String>,
    extra_mounts: Vec<MountDescriptor>,
) -> Value {
    let (stdout, stderr, exit_code) = run_guest_probe_process(
        case_name,
        cwd,
        entrypoint,
        mount_registry_commands,
        extra_metadata,
        extra_mounts,
    );

    assert_eq!(
        exit_code, 0,
        "guest probe failed for {case_name}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.trim().is_empty(),
        "guest probe stderr for {case_name}:\n{stderr}"
    );

    serde_json::from_str(stdout.trim()).expect("parse guest probe JSON")
}

fn write_probe(case_name: &str, script: &str) -> (PathBuf, PathBuf) {
    let cwd = temp_dir(&format!("posix-path-repro-{case_name}"));
    let entrypoint = cwd.join("entry.mjs");
    write_fixture(&entrypoint, script);
    (cwd, entrypoint)
}

fn assert_guest_matches_host(case_name: &str, script: &str) {
    assert_node_available();

    let (cwd, entrypoint) = write_probe(case_name, script);
    let host = run_host_probe(&cwd, &entrypoint);
    let guest = run_guest_probe(
        case_name,
        &cwd,
        &entrypoint,
        false,
        HashMap::new(),
        Vec::new(),
    );

    assert_eq!(
        guest,
        host,
        "guest V8 result diverged from host Node for {case_name}\nhost: {}\nguest: {}",
        serde_json::to_string_pretty(&host).expect("pretty host JSON"),
        serde_json::to_string_pretty(&guest).expect("pretty guest JSON")
    );
}

fn guest_shell_relative_paths_follow_cwd_after_cd() {
    assert_node_available();

    let (cwd, entrypoint) = write_probe(
        "relative-shell",
        r#"
import childProcess from "node:child_process";
import fs from "node:fs";

const worktree = process.env.WORKTREE;
if (!worktree) {
  throw new Error("WORKTREE env missing");
}
const notePath = `${worktree}/note.txt`;
const writtenPath = `${worktree}/written.txt`;
fs.writeFileSync(notePath, "hello from repro\n");
const childScript = `
const fs = require("node:fs");
console.log(process.cwd());
console.log(fs.readFileSync("note.txt", "utf8").trimEnd());
fs.writeFileSync("written.txt", "hi\\n");
console.log(fs.readFileSync("written.txt", "utf8").trimEnd());
`;

const result = childProcess.spawnSync(
  "node",
  [
    "-e",
    childScript,
  ],
  {
    cwd: worktree,
    encoding: "utf8",
  },
);

const stdoutText = Buffer.from(result.stdout ?? []).toString("utf8");
const stderrText = Buffer.from(result.stderr ?? []).toString("utf8");
const stdoutLines = stdoutText
  .split("\n")
  .map((line) => line.trimEnd())
  .filter((line) => line.length > 0);
let written = null;
let writtenReadError = null;
try {
  written = fs.readFileSync(writtenPath, "utf8");
} catch (error) {
  writtenReadError = {
    code: error?.code ?? null,
    path: error?.path ?? null,
  };
}

console.log(JSON.stringify({
  worktree,
  notePath,
  writtenPath,
  status: result.status,
  signal: result.signal,
  stdoutLines,
  stderr: stderrText,
  written,
  writtenReadError,
}));
"#,
    );
    let guest = run_guest_probe(
        "relative-shell",
        &cwd,
        &entrypoint,
        false,
        HashMap::from([(String::from("env.WORKTREE"), String::from("/workspace"))]),
        vec![MountDescriptor {
            guest_path: String::from("/workspace"),
            guest_source: String::from("host_dir"),
            guest_fstype: String::from("host_dir"),
            read_only: false,
            plugin: MountPluginDescriptor {
                id: String::from("host_dir"),
                config: serde_json::to_string(&json!({
                    "hostPath": cwd,
                    "readOnly": false,
                }))
                .expect("serialize relative shell mount config"),
            },
        }],
    );

    assert_eq!(
        guest["status"],
        json!(0),
        "guest repro should exit cleanly: {guest}"
    );
    assert_eq!(
        guest["signal"],
        Value::Null,
        "guest repro should not be signaled: {guest}"
    );
    assert_eq!(
        guest["stdoutLines"],
        json!([
            guest["worktree"]
                .as_str()
                .expect("worktree should be a string"),
            "hello from repro",
            "hi"
        ]),
        "guest shell should resolve relative paths inside the cwd: {guest}"
    );
    assert_eq!(
        strip_benign_child_pid_warnings(
            guest["stderr"]
                .as_str()
                .expect("child stderr should be encoded as a string")
        ),
        "",
        "guest shell should not emit unexpected stderr: {guest}"
    );
    assert_eq!(
        guest["written"],
        json!("hi\n"),
        "relative write should land in the cwd: {guest}"
    );
}

fn guest_shell_absolute_paths_still_work_after_cd() {
    assert_node_available();

    let (cwd, entrypoint) = write_probe(
        "absolute-shell",
        r#"
import childProcess from "node:child_process";
import fs from "node:fs";
import path from "node:path";

const worktree = process.env.WORKTREE;
if (!worktree) {
  throw new Error("WORKTREE env missing");
}

const notePath = path.join(worktree, "note.txt");
const writtenPath = path.join(worktree, "written.txt");
fs.writeFileSync(notePath, "hello from repro\n");
const childScript = `
const fs = require("node:fs");
console.log(process.cwd());
console.log(fs.readFileSync(${JSON.stringify(notePath)}, "utf8").trimEnd());
fs.writeFileSync(${JSON.stringify(writtenPath)}, "hi\\n");
console.log(fs.readFileSync(${JSON.stringify(writtenPath)}, "utf8").trimEnd());
`;

const result = childProcess.spawnSync(
  "node",
  [
    "-e",
    childScript,
  ],
  {
    cwd: worktree,
    encoding: "utf8",
  },
);

const stdoutText = Buffer.from(result.stdout ?? []).toString("utf8");
const stderrText = Buffer.from(result.stderr ?? []).toString("utf8");
const stdoutLines = stdoutText
  .split("\n")
  .map((line) => line.trimEnd())
  .filter((line) => line.length > 0);
let written = null;
let writtenReadError = null;
try {
  written = fs.readFileSync(writtenPath, "utf8");
} catch (error) {
  writtenReadError = {
    code: error?.code ?? null,
    path: error?.path ?? null,
  };
}

console.log(JSON.stringify({
  worktree,
  notePath,
  writtenPath,
  status: result.status,
  signal: result.signal,
  stdoutLines,
  stderr: stderrText,
  written,
  writtenReadError,
}));
"#,
    );
    let guest = run_guest_probe(
        "absolute-shell",
        &cwd,
        &entrypoint,
        false,
        HashMap::from([(String::from("env.WORKTREE"), String::from("/workspace"))]),
        vec![MountDescriptor {
            guest_path: String::from("/workspace"),
            guest_source: String::from("host_dir"),
            guest_fstype: String::from("host_dir"),
            read_only: false,
            plugin: MountPluginDescriptor {
                id: String::from("host_dir"),
                config: serde_json::to_string(&json!({
                    "hostPath": cwd,
                    "readOnly": false,
                }))
                .expect("serialize absolute shell mount config"),
            },
        }],
    );

    assert_eq!(
        guest["status"],
        json!(0),
        "guest repro should exit cleanly: {guest}"
    );
    assert_eq!(
        guest["signal"],
        Value::Null,
        "guest repro should not be signaled: {guest}"
    );
    assert_eq!(
        guest["stdoutLines"],
        json!([
            guest["worktree"]
                .as_str()
                .expect("worktree should be a string"),
            "hello from repro",
            "hi"
        ]),
        "guest shell should still succeed with absolute paths: {guest}"
    );
    assert_eq!(
        strip_benign_child_pid_warnings(
            guest["stderr"]
                .as_str()
                .expect("child stderr should be encoded as a string")
        ),
        "",
        "guest shell should not emit unexpected stderr: {guest}"
    );
    assert_eq!(
        guest["written"],
        json!("hi\n"),
        "absolute write should land in the cwd: {guest}"
    );
}

fn nested_wasm_tokio_shell_accepts_workspace_cwd() {
    assert_node_available();

    let (cwd, entrypoint) = write_probe(
        "nested-wasm-tokio-shell",
        r#"
import childProcess from "node:child_process";

const result = childProcess.spawnSync(
  "spawn-test-host",
  ["tokio-bash"],
  { cwd: "/workspace", encoding: "utf8" },
);

console.log(JSON.stringify({
  status: result.status,
  signal: result.signal,
  stdout: Buffer.from(result.stdout ?? []).toString("utf8"),
  stderr: Buffer.from(result.stderr ?? []).toString("utf8"),
  error: result.error ? String(result.error) : null,
}));
"#,
    );
    let guest = run_guest_probe(
        "nested-wasm-tokio-shell",
        &cwd,
        &entrypoint,
        true,
        HashMap::new(),
        vec![
            MountDescriptor {
                guest_path: String::from("/workspace"),
                guest_source: String::from("host_dir"),
                guest_fstype: String::from("host_dir"),
                read_only: false,
                plugin: MountPluginDescriptor {
                    id: String::from("host_dir"),
                    config: serde_json::to_string(&json!({
                        "hostPath": cwd,
                        "readOnly": false,
                    }))
                    .expect("serialize nested WASM workspace mount config"),
                },
            },
            MountDescriptor {
                guest_path: String::from("/opt/agentos/bin"),
                guest_source: String::from("host_dir"),
                guest_fstype: String::from("host_dir"),
                read_only: true,
                plugin: MountPluginDescriptor {
                    id: String::from("host_dir"),
                    config: serde_json::to_string(&json!({
                        "hostPath": registry_command_root(),
                        "readOnly": true,
                    }))
                    .expect("serialize nested WASM command mount config"),
                },
            },
        ],
    );

    assert_eq!(
        guest["status"],
        json!(0),
        "nested WASM shell failed: {guest}"
    );
    assert_eq!(
        guest["signal"],
        Value::Null,
        "nested WASM shell was signaled: {guest}"
    );
    assert_eq!(
        guest["error"],
        Value::Null,
        "nested WASM spawn errored: {guest}"
    );
    assert_eq!(
        guest["stdout"],
        json!("PASS\n"),
        "nested WASM shell output differed: {guest}"
    );
    assert_eq!(
        strip_benign_child_pid_warnings(
            guest["stderr"]
                .as_str()
                .expect("nested WASM stderr should be a string")
        ),
        "",
        "nested WASM shell emitted unexpected stderr: {guest}"
    );
}

fn node_path_posix_edge_cases_match_host_node() {
    assert_guest_matches_host(
        "path-builtins",
        r#"
import path from "node:path";

console.log(JSON.stringify({
  resolve: path.resolve("/workspace/project/", "./src", "../tests", "spec.ts"),
  join: path.join("/workspace", "project", "..", "project", "note.txt"),
  normalize: path.normalize("/workspace//project/tests/../nested//file.txt"),
  relativeSibling: path.relative("/workspace/project/src/", "/workspace/project/tests/spec.ts"),
  relativeSame: path.relative("/workspace/project/", "/workspace/project"),
  dirname: path.dirname("/workspace/project/tests/spec.ts/"),
  basename: path.basename("/workspace/project/tests/spec.ts/"),
}));
"#,
    );
}

fn filesystem_path_edge_cases_match_host_node() {
    assert_guest_matches_host(
        "filesystem-paths",
        r#"
import fs from "node:fs";
import path from "node:path";

fs.mkdirSync("workspace/nested", { recursive: true });
fs.writeFileSync("workspace/nested/note.txt", "hello through nested path\n");
fs.symlinkSync("nested", "workspace/link");

const viaRelative = fs.readFileSync("./workspace/./nested/../nested/note.txt", "utf8");
const viaSymlinkTraversal = fs.readFileSync("workspace/link/../nested/note.txt", "utf8");
const realpathRelativeToCwd = path.relative(
  process.cwd(),
  fs.realpathSync("workspace/link/../nested/note.txt"),
);
const readlink = fs.readlinkSync("workspace/link");
const trailingSlashEntries = fs.readdirSync("workspace/nested/");
const trailingSlashIsDir = fs.statSync("workspace/nested/").isDirectory();
const lstatIsSymlink = fs.lstatSync("workspace/link").isSymbolicLink();

console.log(JSON.stringify({
  viaRelative,
  viaSymlinkTraversal,
  realpathRelativeToCwd,
  readlink,
  trailingSlashEntries,
  trailingSlashIsDir,
  lstatIsSymlink,
}));
"#,
    );
}

#[test]
fn posix_path_repro_suite() {
    // Multiple libtest cases in this V8-backed integration binary still trip
    // teardown/init crashes, so keep the coverage in one top-level suite.
    filesystem_path_edge_cases_match_host_node();
    guest_shell_absolute_paths_still_work_after_cd();
    guest_shell_relative_paths_follow_cwd_after_cd();
    nested_wasm_tokio_shell_accepts_workspace_cwd();
    node_path_posix_edge_cases_match_host_node();
}
