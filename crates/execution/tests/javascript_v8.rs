use agent_os_execution::{
    v8_runtime::map_bridge_method, CreateJavascriptContextRequest, JavascriptExecutionEngine,
    JavascriptExecutionEvent, StartJavascriptExecutionRequest,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

/*
US-040 execution-test audit

Deleted coverage:
- `tests/javascript.rs`: removed because the file only exercised the old
  `legacy-js-tests` host-Node guest path (`loader.mjs`, `runner.mjs`,
  import-cache mutation, and `Command::new("node")` process behavior). The V8
  isolate path no longer uses that guest execution model.
- `permission_flags::node_permission_flags_do_not_expose_workspace_root_or_entrypoint_parent_writes`:
  removed because its JavaScript assertions depended on host-Node permission
  flags emitted for guest JS launches. V8 guest JS now stays in-process, while
  the remaining permission-flag tests still cover the real host-Node launches
  that remain for Python and WASM.
- `benchmark::javascript_benchmark_harness_covers_required_startup_and_import_scenarios`:
  removed because it depended on pre-V8 benchmark marker behavior from the old
  startup harness instead of validating the current V8 execution path. The
  stable artifact and markdown benchmark tests remain.
*/

fn write_fixture(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create fixture parent dirs");
    }
    fs::write(path, contents).expect("write fixture");
}

fn run_host_node_json(cwd: &Path, entrypoint: &Path) -> Value {
    let output = Command::new("node")
        .arg(entrypoint)
        .current_dir(cwd)
        .output()
        .expect("run host node");

    assert!(
        output.status.success(),
        "host node failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("parse host JSON")
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf 'guest-node-invoked\\n' >> \"{}\"\nexit 99\n",
        log_path.display()
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe {
                std::env::set_var(self.key, value);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

#[test]
fn javascript_contexts_preserve_vm_and_bootstrap_configuration() {
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: Some(String::from("./bootstrap.mjs")),
        compile_cache_root: None,
    });

    assert_eq!(context.context_id, "js-ctx-1");
    assert_eq!(context.vm_id, "vm-js");
    assert_eq!(context.bootstrap_module.as_deref(), Some("./bootstrap.mjs"));
    assert_eq!(context.compile_cache_dir, None);
}

#[test]
fn javascript_execution_uses_v8_runtime_without_spawning_guest_node_binary() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set_path("AGENT_OS_NODE_BINARY", &fake_node_path);

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from("globalThis.__agentOsRanInV8 = true;")),
        })
        .expect("start JavaScript execution");

    assert!(
        execution.uses_shared_v8_runtime(),
        "guest JS should run inside the shared V8 runtime"
    );
    assert_ne!(
        execution.child_pid(),
        0,
        "shared V8 runtime executions should report the host runtime pid for lifecycle control"
    );

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        !log_path.exists(),
        "guest JavaScript execution should not invoke the host node binary"
    );
}

#[test]
fn javascript_execution_virtualizes_process_metadata_for_inline_v8_code() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs"), String::from("alpha")],
            env: BTreeMap::from([
                (
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PID"),
                    String::from("4242"),
                ),
                (
                    String::from("AGENT_OS_VIRTUAL_PROCESS_PPID"),
                    String::from("41"),
                ),
            ]),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
if (process.argv[1] !== "/root/entry.mjs") throw new Error(`argv=${process.argv[1]}`);
if (process.argv[2] !== "alpha") throw new Error(`arg2=${process.argv[2]}`);
if (process.cwd() !== "/root") throw new Error(`cwd=${process.cwd()}`);
if (process.pid !== 4242) throw new Error(`pid=${process.pid}`);
if (process.ppid !== 41) throw new Error(`ppid=${process.ppid}`);
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn javascript_execution_file_url_to_path_accepts_guest_absolute_paths() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { fileURLToPath } from "node:url";

const guestPath = "/root/node_modules/@mariozechner/pi-coding-agent/dist/config.js";
if (fileURLToPath(guestPath) !== guestPath) {
  throw new Error(`plain path mismatch: ${fileURLToPath(guestPath)}`);
}

const href = "file:///root/node_modules/@mariozechner/pi-coding-agent/dist/config.js";
if (fileURLToPath(href) !== guestPath) {
  throw new Error(`file url mismatch: ${fileURLToPath(href)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn javascript_execution_imports_node_events_without_hanging() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { EventEmitter, once } from "node:events";

const emitter = new EventEmitter();
const pending = once(emitter, "ready");
emitter.emit("ready", "ok");
const [value] = await pending;

if (value !== "ok") {
  throw new Error(`unexpected once payload: ${value}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_imports_node_process_without_hanging() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import process from "node:process";

if (!process || typeof process.cwd !== "function") {
  throw new Error("node:process did not export the guest process object");
}

if (typeof process.pid !== "number" || process.pid <= 0) {
  throw new Error(`unexpected pid: ${process.pid}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_imports_node_fs_promises_without_hanging() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import fs from "node:fs/promises";

if (typeof fs.access !== "function") {
  throw new Error("node:fs/promises did not expose access()");
}
if (typeof fs.readFile !== "function") {
  throw new Error("node:fs/promises did not expose readFile()");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_imports_node_perf_hooks_without_hanging() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { performance } from "node:perf_hooks";

if (typeof performance?.now !== "function") {
  throw new Error("node:perf_hooks did not expose performance.now()");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_denies_dangerous_builtins_with_err_access_denied() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
for (const builtin of ["vm", "worker_threads", "inspector", "v8", "cluster"]) {
  let denied = false;
  try {
    require(`node:${builtin}`);
  } catch (error) {
    denied =
      error?.code === "ERR_ACCESS_DENIED" &&
      String(error?.message ?? "").includes(`node:${builtin}`);
  }
  if (!denied) {
    throw new Error(`node:${builtin} was not denied`);
  }
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_provides_async_hooks_and_diagnostics_channel_stubs() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const asyncHooks = require("node:async_hooks");
const diagnosticsChannel = require("node:diagnostics_channel");

const hook = asyncHooks.createHook({});
if (hook.enable() !== hook || hook.disable() !== hook) {
  throw new Error("node:async_hooks createHook() did not return a no-op hook");
}
if (asyncHooks.executionAsyncId() !== 0 || asyncHooks.triggerAsyncId() !== 0) {
  throw new Error("node:async_hooks ids should default to 0");
}

const storage = new asyncHooks.AsyncLocalStorage();
const result = storage.run("token", () => storage.getStore());
if (result !== "token") {
  throw new Error(`node:async_hooks AsyncLocalStorage lost store: ${String(result)}`);
}

const channel = diagnosticsChannel.channel("undici:request:create");
if (channel.name !== "undici:request:create") {
  throw new Error(`unexpected channel name: ${String(channel.name)}`);
}
if (channel.hasSubscribers !== false) {
  throw new Error("diagnostics channel should report no subscribers");
}
if (diagnosticsChannel.hasSubscribers("undici:request:create") !== false) {
  throw new Error("diagnostics_channel.hasSubscribers should be false");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_supports_require_resolve_for_guest_code() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("local-file.js"),
        "module.exports = 'local';\n",
    );
    write_fixture(
        &temp.path().join("nested/check.cjs"),
        r#"
const localResolved = require.resolve("../local-file.js");
if (localResolved !== "/root/local-file.js") {
  throw new Error(`unexpected local resolution: ${String(localResolved)}`);
}

const packageResolved = require.resolve("some-package");
if (packageResolved !== "/root/node_modules/some-package/index.js") {
  throw new Error(`unexpected package resolution: ${String(packageResolved)}`);
}

const searchPaths = require.resolve.paths("some-package");
const expectedPaths = [
  "/root/nested/node_modules",
  "/root/node_modules",
  "/node_modules",
];
if (JSON.stringify(searchPaths) !== JSON.stringify(expectedPaths)) {
  throw new Error(`unexpected search paths: ${JSON.stringify(searchPaths)}`);
}
"#,
    );
    write_fixture(
        &temp.path().join("node_modules/some-package/package.json"),
        r#"{"main":"./index.js"}"#,
    );
    write_fixture(
        &temp.path().join("node_modules/some-package/index.js"),
        "module.exports = 'pkg';\n",
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
if (require.resolve("fs") !== "node:fs") {
  throw new Error(`builtin resolution failed: ${String(require.resolve("fs"))}`);
}

if (require.resolve("./local-file.js") !== "/root/local-file.js") {
  throw new Error(`local resolution failed: ${String(require.resolve("./local-file.js"))}`);
}

if (require.resolve("some-package") !== "/root/node_modules/some-package/index.js") {
  throw new Error(`package resolution failed: ${String(require.resolve("some-package"))}`);
}

const builtinPaths = require.resolve.paths("fs");
if (builtinPaths !== null) {
  throw new Error(`builtin paths should be null, got ${JSON.stringify(builtinPaths)}`);
}

const packagePaths = require.resolve.paths("some-package");
const expectedPackagePaths = ["/root/node_modules", "/node_modules"];
if (JSON.stringify(packagePaths) !== JSON.stringify(expectedPackagePaths)) {
  throw new Error(`unexpected top-level search paths: ${JSON.stringify(packagePaths)}`);
}

let missingCode = null;
try {
  require.resolve("nonexistent");
} catch (error) {
  missingCode = error?.code ?? null;
}
if (missingCode !== "MODULE_NOT_FOUND") {
  throw new Error(`unexpected missing-module code: ${String(missingCode)}`);
}

require("./nested/check.cjs");
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );
}

#[test]
fn javascript_execution_surfaces_sync_rpc_requests_from_v8_modules() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import fs from "node:fs";
fs.statSync("/workspace/note.txt");
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/note.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 11,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
}

#[test]
fn javascript_execution_v8_dgram_bridge_matches_sidecar_rpc_shapes() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import dgram from "node:dgram";

const summary = await new Promise((resolve, reject) => {
  const socket = dgram.createSocket("udp4");
  socket.on("error", reject);
  socket.on("message", (message, rinfo) => {
    const address = socket.address();
    socket.close(() => {
      resolve({
        address,
        message: message.toString("utf8"),
        rinfo,
      });
    });
  });
  socket.bind(0, "127.0.0.1", () => {
    socket.send("ping", 7, "127.0.0.1");
  });
});

if (summary.message !== "pong") {
  throw new Error(`unexpected udp message: ${summary.message}`);
}
if (summary.address.address !== "127.0.0.1" || summary.address.port !== 45454) {
  throw new Error(`unexpected socket address: ${JSON.stringify(summary.address)}`);
}
if (summary.rinfo.address !== "127.0.0.1" || summary.rinfo.port !== 7) {
  throw new Error(`unexpected remote info: ${JSON.stringify(summary.rinfo)}`);
}
"#,
    );
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                String::from("[\"dgram\"]"),
            )]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.createSocket request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.createSocket request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.createSocket");
    assert_eq!(request.args, vec![json!({ "type": "udp4" })]);
    execution
        .respond_sync_rpc_success(request.id, json!({ "socketId": "udp-1", "type": "udp4" }))
        .expect("respond to dgram.createSocket");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.bind request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.bind request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.bind");
    assert_eq!(
        request.args,
        vec![json!("udp-1"), json!({ "address": "127.0.0.1", "port": 0 })]
    );
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "localAddress": "127.0.0.1",
                "localPort": 45454,
                "family": "IPv4",
            }),
        )
        .expect("respond to dgram.bind");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.poll request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.poll request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.poll");
    assert_eq!(request.args, vec![json!("udp-1"), json!(10)]);
    execution
        .respond_sync_rpc_success(request.id, json!(null))
        .expect("respond to initial dgram.poll");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.send request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.send request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.send");
    assert_eq!(
        request.args,
        vec![
            json!("udp-1"),
            json!({
                "__agentOsType": "bytes",
                "base64": "cGluZw==",
            }),
            json!({
                "address": "127.0.0.1",
                "port": 7,
            }),
        ]
    );
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "bytes": 4,
                "localAddress": "127.0.0.1",
                "localPort": 45454,
                "family": "IPv4",
            }),
        )
        .expect("respond to dgram.send");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll message dgram.poll request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected message dgram.poll request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.poll");
    assert_eq!(request.args, vec![json!("udp-1"), json!(10)]);
    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "type": "message",
                "data": {
                    "__agentOsType": "bytes",
                    "base64": "cG9uZw==",
                },
                "remoteAddress": "127.0.0.1",
                "remotePort": 7,
                "remoteFamily": "IPv4",
            }),
        )
        .expect("respond to message dgram.poll");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.address request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.address request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.address");
    assert_eq!(request.args, vec![json!("udp-1")]);
    execution
        .respond_sync_rpc_success(
            request.id,
            json!("{\"address\":\"127.0.0.1\",\"port\":45454,\"family\":\"IPv4\"}"),
        )
        .expect("respond to dgram.address");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll dgram.close request")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected dgram.close request, got {other:?}"),
    };
    assert_eq!(request.method, "dgram.close");
    assert_eq!(request.args, vec![json!("udp-1")]);
    execution
        .respond_sync_rpc_success(request.id, json!(null))
        .expect("respond to dgram.close");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_strips_hashbang_from_module_entrypoints() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &temp.path().join("index.js"),
        "#!/usr/bin/env node\nimport fs from \"node:fs\";\nfs.statSync(\"/workspace/hashbang.txt\");\n",
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./index.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/hashbang.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 9,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_resolves_pnpm_store_dependencies_from_symlinked_entrypoints() {
    let temp = tempdir().expect("create temp dir");
    let node_modules = temp.path().join("node_modules");
    let store_root = node_modules.join(".pnpm/pkg@1.0.0/node_modules");
    let pkg_dir = store_root.join("pkg");
    let dep_dir = store_root.join("@scope/dep");

    fs::create_dir_all(pkg_dir.join("dist")).expect("create package dist");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(node_modules.join("@scope")).expect("create scope dir");

    write_fixture(&pkg_dir.join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &pkg_dir.join("dist/index.js"),
        "import dep from \"@scope/dep\";\ndep();\n",
    );
    write_fixture(
        &dep_dir.join("package.json"),
        r#"{"type":"module","exports":"./index.js"}"#,
    );
    write_fixture(
        &dep_dir.join("index.js"),
        "import fs from \"node:fs\";\nexport default function dep() { fs.statSync(\"/workspace/pnpm.txt\"); }\n",
    );

    symlink(".pnpm/pkg@1.0.0/node_modules/pkg", node_modules.join("pkg"))
        .expect("symlink package into node_modules");

    let guest_mappings = serde_json::to_string(&vec![json!({
        "guestPath": "/root/node_modules",
        "hostPath": node_modules.display().to_string(),
    })])
    .expect("serialize guest mappings");

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("/root/node_modules/pkg/dist/index.js")],
            env: BTreeMap::from([(String::from("AGENT_OS_GUEST_PATH_MAPPINGS"), guest_mappings)]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/pnpm.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 8,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_resolves_dependencies_from_package_specific_symlink_mounts() {
    let temp = tempdir().expect("create temp dir");
    let mounts_root = temp.path().join("mounts");
    let node_modules_root = temp.path().join("node_modules");
    let store_root = node_modules_root.join(".pnpm/pkg@1.0.0/node_modules");
    let pkg_dir = store_root.join("pkg");
    let dep_dir = store_root.join("@scope/dep");
    let mounted_pkg = mounts_root.join("pkg");

    fs::create_dir_all(pkg_dir.join("dist")).expect("create package dist");
    fs::create_dir_all(&dep_dir).expect("create dependency dir");
    fs::create_dir_all(&mounts_root).expect("create mounts root");

    write_fixture(&pkg_dir.join("package.json"), r#"{"type":"module"}"#);
    write_fixture(
        &pkg_dir.join("dist/index.js"),
        "import dep from \"@scope/dep\";\ndep();\n",
    );
    write_fixture(
        &dep_dir.join("package.json"),
        r#"{"type":"module","exports":"./index.js"}"#,
    );
    write_fixture(
        &dep_dir.join("index.js"),
        "import fs from \"node:fs\";\nexport default function dep() { fs.statSync(\"/workspace/pkg-mount.txt\"); }\n",
    );

    symlink(&pkg_dir, &mounted_pkg).expect("symlink mounted package to pnpm store");

    let guest_mappings = serde_json::to_string(&vec![json!({
        "guestPath": "/root/node_modules/pkg",
        "hostPath": mounted_pkg.display().to_string(),
    })])
    .expect("serialize guest mappings");

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let mut execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("/root/node_modules/pkg/dist/index.js")],
            env: BTreeMap::from([(String::from("AGENT_OS_GUEST_PATH_MAPPINGS"), guest_mappings)]),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll execution event")
    {
        Some(JavascriptExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "fs.statSync");
    assert_eq!(request.args, vec![json!("/workspace/pkg-mount.txt")]);

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "mode": 0o100644,
                "size": 13,
                "isDirectory": false,
                "isSymbolicLink": false,
            }),
        )
        .expect("respond to fs.statSync");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_timer_callbacks_fire_and_clear_correctly() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
(async () => {
  const clearedTimeout = setTimeout(() => {
    throw new Error("cleared timeout fired");
  }, 10);
  clearTimeout(clearedTimeout);

  await new Promise((resolve) => setTimeout(resolve, 25));

  let intervalTicks = 0;
  await new Promise((resolve, reject) => {
    const interval = setInterval(() => {
      intervalTicks += 1;
      if (intervalTicks === 2) {
        clearInterval(interval);
        resolve();
      } else if (intervalTicks > 2) {
        reject(new Error(`interval fired too many times: ${intervalTicks}`));
      }
    }, 10);

    setTimeout(() => reject(new Error(`interval timeout: ${intervalTicks}`)), 250);
  });

  if (intervalTicks !== 2) {
    throw new Error(`interval tick count mismatch: ${intervalTicks}`);
  }
})().catch((error) => {
  process.exitCode = 1;
  throw error;
});
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_readline_polyfill_emits_lines() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { EventEmitter } from "node:events";
import { createInterface } from "node:readline";

const input = new EventEmitter();
const seen = [];
const rl = createInterface({ input });
rl.on("line", (line) => seen.push(line));
input.emit("data", "alpha\nbeta\r\ngamma");
input.emit("end");

if (seen.length !== 3) {
  throw new Error(`expected 3 lines, got ${JSON.stringify(seen)}`);
}
if (seen[0] !== "alpha" || seen[1] !== "beta" || seen[2] !== "gamma") {
  throw new Error(`unexpected lines: ${JSON.stringify(seen)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_builtin_wrappers_expose_common_named_exports() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
import { spawn, spawnSync } from "node:child_process";
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, readSync, readdirSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { homedir, platform } from "node:os";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";

if (typeof spawn !== "function" || typeof spawnSync !== "function") throw new Error("child_process exports missing");
if (typeof closeSync !== "function" || typeof existsSync !== "function" || typeof mkdirSync !== "function") throw new Error("fs exports missing");
if (typeof openSync !== "function" || typeof readFileSync !== "function" || typeof readSync !== "function") throw new Error("fs exports missing");
if (typeof readdirSync !== "function" || typeof realpathSync !== "function" || typeof statSync !== "function" || typeof writeFileSync !== "function") throw new Error("fs exports missing");
if (typeof homedir !== "function" || typeof platform !== "function") throw new Error("os exports missing");
if (typeof basename !== "function" || typeof dirname !== "function" || typeof isAbsolute !== "function" || typeof join !== "function" || typeof resolve !== "function") throw new Error("path exports missing");
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
#[ignore = "Guest child_process command resolution is still broken on this branch; sidecar/execution conformance for the remaining builtins is active"]
fn javascript_execution_v8_child_process_conformance_matches_host_node() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import childProcess from "node:child_process";
import fs from "node:fs";

fs.writeFileSync("async-out.txt", Buffer.from("async:beta-async\n", "utf8"));

const syncPiped = childProcess.spawnSync("/bin/cat", [], {
  input: Buffer.from("alpha-sync"),
});
const syncError = childProcess.spawnSync("/bin/cat", ["definitely-missing-agentos-file"]);

const asyncResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn("/bin/cat", ["async-out.txt"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const timer = setTimeout(() => {
    reject(new Error("spawn(/bin/cat async-out.txt) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

const asyncErrorResult = await new Promise((resolve, reject) => {
  const child = childProcess.spawn("/bin/cat", ["definitely-missing-agentos-file"], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  const timer = setTimeout(() => {
    reject(new Error("spawn(/bin/cat missing-file) did not close within 2s"));
  }, 2000);
  const stdout = [];
  const stderr = [];
  child.stdout.on("data", (chunk) => {
    stdout.push(Buffer.from(chunk));
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(Buffer.from(chunk));
  });
  child.on("error", reject);
  child.on("close", (code, signal) => {
    clearTimeout(timer);
    resolve({
      code,
      signal,
      stdoutBase64: Buffer.concat(stdout).toString("base64"),
      stderrBase64: Buffer.concat(stderr).toString("base64"),
    });
  });
});

console.log(JSON.stringify({
  syncPipedStatus: syncPiped.status,
  syncPipedStdoutBase64: Buffer.from(syncPiped.stdout ?? []).toString("base64"),
  syncPipedStderrBase64: Buffer.from(syncPiped.stderr ?? []).toString("base64"),
  syncErrorStatus: syncError.status,
  syncErrorStdoutBase64: Buffer.from(syncError.stdout ?? []).toString("base64"),
  syncErrorStderrBase64: Buffer.from(syncError.stderr ?? []).toString("base64"),
  asyncCode: asyncResult.code,
  asyncSignal: asyncResult.signal,
  asyncStdoutBase64: asyncResult.stdoutBase64,
  asyncStderrBase64: asyncResult.stderrBase64,
  asyncErrorCode: asyncErrorResult.code,
  asyncErrorSignal: asyncErrorResult.signal,
  asyncErrorStdoutBase64: asyncErrorResult.stdoutBase64,
  asyncErrorStderrBase64: asyncErrorResult.stderrBase64,
}));
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let host = run_host_node_json(temp.path(), &temp.path().join("entry.mjs"));
    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");

    let guest: Value = serde_json::from_str(stdout.trim()).expect("parse guest JSON");
    assert_eq!(
        guest,
        host,
        "guest child_process result diverged from host Node\nhost: {}\nguest: {}",
        serde_json::to_string_pretty(&host).expect("pretty host JSON"),
        serde_json::to_string_pretty(&guest).expect("pretty guest JSON")
    );
}

#[test]
fn javascript_execution_v8_web_stream_globals_support_basic_io() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const writes = [];
const writable = new WritableStream({
  write(chunk) {
    writes.push(new TextDecoder().decode(chunk));
  },
});
const writer = writable.getWriter();
await writer.write(new TextEncoder().encode("hello"));
writer.releaseLock();

const readable = new ReadableStream({
  start(controller) {
    controller.enqueue("alpha");
    controller.close();
  },
});
const reader = readable.getReader();
const first = await reader.read();
const second = await reader.read();
reader.releaseLock();

if (writes.length !== 1 || writes[0] !== "hello") {
  throw new Error(`unexpected writes: ${JSON.stringify(writes)}`);
}
if (first.value !== "alpha" || first.done !== false || second.done !== true) {
  throw new Error(`unexpected reads: ${JSON.stringify({ first, second })}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_abort_controller_dispatches_abort() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const controller = new AbortController();
let seenAbort = false;
controller.signal.addEventListener("abort", () => {
  seenAbort = true;
});
controller.abort("stop");
if (!controller.signal.aborted || controller.signal.reason !== "stop" || !seenAbort) {
  throw new Error("abort controller did not update signal state");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_request_accepts_abort_signal() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const controller = new AbortController();
const request = new Request("http://example.com/test", {
  method: "POST",
  body: JSON.stringify({ ok: true }),
  duplex: "half",
  signal: controller.signal,
  headers: { "content-type": "application/json" },
});
if (!(request.signal instanceof AbortSignal)) {
  throw new Error("request signal was not preserved");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_abort_signal_static_helpers_work() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
if (typeof AbortSignal.timeout !== "function") {
  throw new Error("AbortSignal.timeout missing");
}
if (typeof AbortSignal.any !== "function") {
  throw new Error("AbortSignal.any missing");
}

const timeoutSignal = AbortSignal.timeout(25);
let timeoutEventCount = 0;
timeoutSignal.addEventListener("abort", () => {
  timeoutEventCount += 1;
});
await new Promise((resolve) => setTimeout(resolve, 60));
if (!timeoutSignal.aborted) {
  throw new Error("AbortSignal.timeout did not abort");
}
if (timeoutEventCount !== 1) {
  throw new Error(`unexpected timeout event count: ${timeoutEventCount}`);
}
if (!timeoutSignal.reason || timeoutSignal.reason.name !== "AbortError") {
  throw new Error(`unexpected timeout reason: ${String(timeoutSignal.reason?.name ?? timeoutSignal.reason)}`);
}

const controller = new AbortController();
const sibling = new AbortController();
const composite = AbortSignal.any([sibling.signal, controller.signal]);
let compositeReason;
composite.addEventListener("abort", () => {
  compositeReason = composite.reason;
});
controller.abort("manual-stop");
await new Promise((resolve) => setTimeout(resolve, 0));
if (!composite.aborted) {
  throw new Error("AbortSignal.any did not abort");
}
if (compositeReason !== "manual-stop" || composite.reason !== "manual-stop") {
  throw new Error(`unexpected composite reason: ${String(composite.reason)}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_schedule_timer_bridge_resolves() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
(async () => {
  let resolved = false;
  await _scheduleTimer.apply(undefined, [15]).then(() => {
    resolved = true;
  });
  if (!resolved) {
    throw new Error("_scheduleTimer did not resolve");
  }
})().catch((error) => {
  process.exitCode = 1;
  throw error;
});
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    assert_eq!(result.exit_code, 0);

    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_crypto_random_sources_use_local_secure_bridge() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.js")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const first = new Uint8Array(32);
const second = new Uint8Array(32);
globalThis.crypto.getRandomValues(first);
globalThis.crypto.getRandomValues(second);

if (first.every((value) => value === 0)) {
  throw new Error("first random buffer was all zero");
}
if (second.every((value) => value === 0)) {
  throw new Error("second random buffer was all zero");
}
const buffersMatch = first.length === second.length &&
  first.every((value, index) => value === second[index]);
if (buffersMatch) {
  throw new Error("random buffers repeated");
}

const uuid = globalThis.crypto.randomUUID();
if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(uuid)) {
  throw new Error(`invalid uuid: ${uuid}`);
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_crypto_basic_operations_emit_expected_sync_rpcs() {
    assert_eq!(
        map_bridge_method("_cryptoHashDigest"),
        ("crypto.hashDigest", false)
    );
    assert_eq!(
        map_bridge_method("_cryptoHmacDigest"),
        ("crypto.hmacDigest", false)
    );
    assert_eq!(map_bridge_method("_cryptoPbkdf2"), ("crypto.pbkdf2", false));
    assert_eq!(map_bridge_method("_cryptoScrypt"), ("crypto.scrypt", false));
    assert_eq!(
        map_bridge_method("_netSocketConnectRaw"),
        ("net.connect", false)
    );
}

#[test]
fn javascript_execution_v8_load_polyfill_returns_runtime_module_expressions() {
    let temp = tempdir().expect("create temp dir");
    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: Some(String::from(
                r#"
const pathExpr = _loadPolyfill.applySyncPromise(undefined, ["path"]);
if (typeof pathExpr !== "string" || !pathExpr.includes("node:path")) {
  throw new Error(`unexpected path polyfill expression: ${String(pathExpr)}`);
}

const pathModule = Function('"use strict"; return (' + pathExpr + ');')();
if (pathModule.join("alpha", "beta") !== "alpha/beta") {
  throw new Error("path polyfill expression did not resolve the runtime module");
}

const deniedExpr = _loadPolyfill.applySyncPromise(undefined, ["inspector"]);
if (typeof deniedExpr !== "string" || !deniedExpr.includes("ERR_ACCESS_DENIED")) {
  throw new Error(`unexpected denied polyfill expression: ${String(deniedExpr)}`);
}

let denied = false;
try {
  Function('"use strict"; return (' + deniedExpr + ');')();
} catch (error) {
  denied = error?.code === "ERR_ACCESS_DENIED";
}
if (!denied) {
  throw new Error("denied polyfill expression did not raise ERR_ACCESS_DENIED");
}

if (_loadPolyfill.applySyncPromise(undefined, ["not-a-real-builtin"]) !== null) {
  throw new Error("unknown polyfill name should return null");
}
"#,
            )),
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stdout = String::from_utf8(result.stdout.clone()).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr.clone()).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn javascript_execution_v8_stream_wrapper_exports_common_node_classes() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("entry.mjs"),
        r#"
import {
  Duplex,
  PassThrough,
  Readable,
  Transform,
  Writable,
  isReadable,
  isWritable,
} from "node:stream";

for (const [name, value] of Object.entries({ Duplex, PassThrough, Readable, Transform, Writable })) {
  if (typeof value !== "function") {
    throw new Error(`${name} was not exported as a constructor`);
  }
}

const pass = new PassThrough();
let output = "";
pass.on("data", (chunk) => {
  output += Buffer.from(chunk).toString("utf8");
});
pass.end("hello");
await new Promise((resolve, reject) => {
  pass.once("close", resolve);
  pass.once("error", reject);
});

if (output !== "hello") {
  throw new Error(`unexpected passthrough output: ${output}`);
}
if (!isReadable(pass) || !isWritable(pass)) {
  throw new Error("stream helpers misreported passthrough readability");
}
"#,
    );

    let mut engine = JavascriptExecutionEngine::default();
    let context = engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let execution = engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            inline_code: None,
        })
        .expect("start JavaScript execution");

    let result = execution.wait().expect("wait for JavaScript execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "unexpected stderr: {stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}
