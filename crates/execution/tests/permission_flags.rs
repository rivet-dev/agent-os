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

const ARG_PREFIX: &str = "ARG=";
const ENV_PREFIX: &str = "ENV=";
const INVOCATION_BREAK: &str = "--END--";
const NODE_ALLOW_WASI_FLAG: &str = "--allow-wasi";
const NODE_WASM_MAX_MEM_PAGES_FLAG_PREFIX: &str = "--wasm-max-mem-pages=";
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
        "#!/bin/sh\nset -eu\nlog=\"{}\"\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> \"$log\"\ndone\nfor key in {} {}; do\n  value=$(printenv \"$key\" || true)\n  if [ -n \"$value\" ]; then\n    printf 'ENV=%s=%s\\n' \"$key\" \"$value\" >> \"$log\"\n  fi\ndone\nprintf '%s\\n' '{}' >> \"$log\"\nexit 0\n",
        log_path.display(),
        WASM_MAX_FUEL_ENV,
        WASM_MAX_MEMORY_BYTES_ENV,
        INVOCATION_BREAK,
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

fn parse_invocations(log_path: &Path) -> Vec<Vec<String>> {
    let contents = fs::read_to_string(log_path).expect("read invocation log");
    let separator = format!("{INVOCATION_BREAK}\n");
    contents
        .split(&separator)
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.strip_prefix(ARG_PREFIX))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn parse_invocation_env(log_path: &Path) -> Vec<BTreeMap<String, String>> {
    let contents = fs::read_to_string(log_path).expect("read invocation log");
    let separator = format!("{INVOCATION_BREAK}\n");
    contents
        .split(&separator)
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            block
                .lines()
                .filter_map(|line| line.strip_prefix(ENV_PREFIX))
                .filter_map(|entry| entry.split_once('='))
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect::<BTreeMap<_, _>>()
        })
        .collect()
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
fn wasm_execution_passes_runtime_memory_and_fuel_limits_to_node_process() {
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let wasm_cwd = temp.path().join("wasm-project");
    fs::create_dir_all(&wasm_cwd).expect("create wasm cwd");
    fs::write(wasm_cwd.join("guest.wasm"), b"\0asm\x01\0\0\0").expect("write wasm module");

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
                (String::from(WASM_MAX_FUEL_ENV), String::from("25")),
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

    let invocations = parse_invocations(&log_path);
    let envs = parse_invocation_env(&log_path);
    assert_eq!(
        invocations.len(),
        2,
        "expected prewarm and execution invocations"
    );
    assert_eq!(
        envs.len(),
        2,
        "expected one env capture per prewarm and execution invocation"
    );

    for (args, env) in invocations.iter().zip(envs.iter()) {
        assert!(
            args.iter()
                .any(|arg| arg == &format!("{NODE_WASM_MAX_MEM_PAGES_FLAG_PREFIX}2")),
            "wasm invocations should enforce the configured runtime page limit: {args:?}"
        );
        assert_eq!(
            env.get(WASM_MAX_MEMORY_BYTES_ENV).map(String::as_str),
            Some("131072"),
            "wasm invocations should receive the configured memory limit env: {env:?}"
        );
        assert_eq!(
            env.get(WASM_MAX_FUEL_ENV).map(String::as_str),
            Some("25"),
            "wasm invocations should receive the configured fuel limit env: {env:?}"
        );
    }
}

#[test]
fn wasm_permission_tiers_only_enable_wasi_outside_isolated_mode() {
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
        fs::write(wasm_cwd.join("guest.wasm"), b"\0asm\x01\0\0\0").expect("write wasm module");

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

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        tiers.len() * 2,
        "expected prewarm and execution invocations for each tier"
    );

    for (index, tier) in tiers.iter().enumerate() {
        for args in &invocations[index * 2..index * 2 + 2] {
            let has_wasi_flag = args.iter().any(|arg| arg == NODE_ALLOW_WASI_FLAG);
            assert_eq!(
                has_wasi_flag,
                !matches!(tier, WasmPermissionTier::Isolated),
                "unexpected --allow-wasi flag for tier {tier:?}: {args:?}"
            );
        }
    }
}
