use agent_os_execution::{
    v8_runtime::map_bridge_method, CreateJavascriptContextRequest, JavascriptExecutionEngine,
    JavascriptExecutionEvent, StartJavascriptExecutionRequest,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
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
    fs::write(path, contents).expect("write fixture");
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

    assert_eq!(execution.child_pid(), 0, "guest JS should run inside V8");

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
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
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
if (Buffer.from(first).equals(Buffer.from(second))) {
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
    assert_eq!(result.exit_code, 0);
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
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
}
