#![cfg(unix)]

use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecutionEngine, PythonExecutionEngine, StartJavascriptExecutionRequest,
    StartPythonExecutionRequest, StartWasmExecutionRequest, WasmExecutionEngine,
    WasmPermissionTier,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::tempdir;

const PYTHON_MAX_OLD_SPACE_MB_ENV: &str = "AGENT_OS_PYTHON_MAX_OLD_SPACE_MB";
const WASM_MAX_FUEL_ENV: &str = "AGENT_OS_WASM_MAX_FUEL";
const WASM_MAX_MEMORY_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_MEMORY_BYTES";

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: This test binary controls its own process environment and uses a
        // single test to avoid concurrent environment mutation within the process.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => {
                // SAFETY: See EnvVarGuard::set; restoring the test process env is
                // limited to this single-threaded test scope.
                unsafe {
                    std::env::set_var(self.key, value);
                }
            }
            None => {
                // SAFETY: See EnvVarGuard::set; restoring the test process env is
                // limited to this single-threaded test scope.
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf 'host-node-invoked\\n' >> \"{}\"\nexit 1\n",
        log_path.display(),
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

fn wasm_noop_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (memory (export "memory") 1)
  (func (export "_start"))
)
"#,
    )
    .expect("compile noop wasm fixture")
}

#[test]
fn node_permission_flags_allow_workers_for_internal_javascript_loader_runtime() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    fs::create_dir_all(&js_cwd).expect("create js cwd");
    fs::write(js_cwd.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = JavascriptExecutionEngine::default();
    let context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let default_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: js_cwd.clone(),
            inline_code: None,
        })
        .expect("start javascript execution without workers")
        .wait()
        .expect("wait for javascript execution without workers");
    assert_eq!(default_result.exit_code, 0);

    let worker_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::from([(
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                String::from("[\"worker_threads\"]"),
            )]),
            cwd: js_cwd,
            inline_code: None,
        })
        .expect("start javascript execution with workers")
        .wait()
        .expect("wait for javascript execution with workers");
    assert_eq!(worker_result.exit_code, 0);

    assert!(
        !log_path.exists(),
        "javascript execution should stay inside the V8 runtime, not spawn the host node binary"
    );
}

#[test]
fn node_permission_flags_only_propagate_nested_child_capabilities_when_parent_explicitly_allows_them(
) {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    fs::create_dir_all(&js_cwd).expect("create js cwd");
    fs::write(js_cwd.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = JavascriptExecutionEngine::default();
    let context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from("vm-js"),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let nested_env = |allow_child_process: &str, allow_worker: &str| {
        BTreeMap::from([
            (
                String::from("AGENT_OS_ALLOWED_NODE_BUILTINS"),
                String::from("[\"child_process\",\"worker_threads\"]"),
            ),
            (
                String::from("AGENT_OS_PARENT_NODE_ALLOW_CHILD_PROCESS"),
                allow_child_process.to_owned(),
            ),
            (
                String::from("AGENT_OS_PARENT_NODE_ALLOW_WORKER"),
                allow_worker.to_owned(),
            ),
        ])
    };

    let denied_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id.clone(),
            argv: vec![String::from("./entry.mjs")],
            env: nested_env("0", "0"),
            cwd: js_cwd.clone(),
            inline_code: None,
        })
        .expect("start nested javascript execution without inherited permissions")
        .wait()
        .expect("wait for nested javascript execution without inherited permissions");
    assert_eq!(denied_result.exit_code, 0);

    let allowed_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from("vm-js"),
            context_id: context.context_id,
            argv: vec![String::from("./entry.mjs")],
            env: nested_env("1", "1"),
            cwd: js_cwd,
            inline_code: None,
        })
        .expect("start nested javascript execution with inherited permissions")
        .wait()
        .expect("wait for nested javascript execution with inherited permissions");
    assert_eq!(allowed_result.exit_code, 0);

    assert!(
        !log_path.exists(),
        "nested javascript execution should stay inside the V8 runtime regardless of inherited node flags"
    );
}

#[test]
fn python_execution_applies_configured_heap_limit_to_v8_runtime() {
    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide-dist");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dist dir");
    fs::write(
        pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide() {
  const v8 = await import("node:v8");
  const heapLimit = v8.getHeapStatistics().heap_size_limit;
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      console.log(String(heapLimit));
    },
  };
}
"#,
    )
    .expect("write pyodide fixture");
    fs::write(pyodide_dir.join("pyodide-lock.json"), "{\"packages\":[]}\n")
        .expect("write pyodide lock fixture");
    for asset in ["pyodide.asm.js", "pyodide.asm.wasm", "python_stdlib.zip"] {
        fs::write(pyodide_dir.join(asset), []).expect("write pyodide runtime fixture");
    }

    let mut python_engine = PythonExecutionEngine::default();
    let context = python_engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let result = python_engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('heap limit')"),
            file_path: None,
            env: BTreeMap::from([(
                String::from(PYTHON_MAX_OLD_SPACE_MB_ENV),
                String::from("64"),
            )]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start python execution")
        .wait(None)
        .expect("wait for python execution");

    assert_eq!(result.exit_code, 0);
    let heap_limit = String::from_utf8(result.stdout)
        .expect("stdout utf8")
        .trim()
        .parse::<u64>()
        .expect("parse heap limit");
    assert!(
        heap_limit >= 16 * 1024 * 1024 && heap_limit < 256 * 1024 * 1024,
        "expected configured Python heap limit to shape the V8 isolate, got {heap_limit} bytes",
    );
}

#[test]
fn wasm_execution_applies_runtime_memory_and_fuel_limits_inside_v8_runtime() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let wasm_cwd = temp.path().join("wasm-project");
    fs::create_dir_all(&wasm_cwd).expect("create wasm cwd");
    fs::write(wasm_cwd.join("guest.wasm"), wasm_noop_module()).expect("write wasm module");

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let result = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: vec![String::from("./guest.wasm")],
            env: BTreeMap::from([
                (String::from(WASM_MAX_FUEL_ENV), String::from("250000")),
                (
                    String::from(WASM_MAX_MEMORY_BYTES_ENV),
                    String::from("131072"),
                ),
            ]),
            cwd: wasm_cwd,
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution")
        .wait()
        .expect("wait for wasm execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        !log_path.exists(),
        "wasm execution should apply runtime limits inside the shared V8 runtime, not launch the host node binary"
    );
}

#[test]
fn wasm_permission_tiers_do_not_fall_back_to_host_node_binary() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let mut engine = WasmExecutionEngine::default();
    let tiers = [
        WasmPermissionTier::Isolated,
        WasmPermissionTier::ReadOnly,
        WasmPermissionTier::ReadWrite,
        WasmPermissionTier::Full,
    ];

    for tier in tiers {
        let tier_name = match tier {
            WasmPermissionTier::Isolated => "isolated",
            WasmPermissionTier::ReadOnly => "read-only",
            WasmPermissionTier::ReadWrite => "read-write",
            WasmPermissionTier::Full => "full",
        };
        let wasm_cwd = temp.path().join(format!("wasm-{tier_name}"));
        fs::create_dir_all(&wasm_cwd).expect("create tier-specific wasm cwd");
        fs::write(wasm_cwd.join("guest.wasm"), wasm_noop_module()).expect("write wasm module");

        let context = engine.create_context(CreateWasmContextRequest {
            vm_id: String::from("vm-wasm"),
            module_path: Some(String::from("./guest.wasm")),
        });

        let result = engine
            .start_execution(StartWasmExecutionRequest {
                vm_id: String::from("vm-wasm"),
                context_id: context.context_id,
                argv: vec![String::from("./guest.wasm")],
                env: BTreeMap::new(),
                cwd: wasm_cwd,
                permission_tier: tier,
            })
            .expect("start wasm execution")
            .wait()
            .expect("wait for wasm execution");
        assert_eq!(result.exit_code, 0);
    }
    assert!(
        !log_path.exists(),
        "wasm permission tiers should stay inside the V8 runtime rather than falling back to the host node binary"
    );
}
