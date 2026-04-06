#![cfg(unix)]

use agent_os_execution::wasm::WASM_MAX_STACK_BYTES_ENV;
use agent_os_execution::{
    CreateJavascriptContextRequest, CreatePythonContextRequest, CreateWasmContextRequest,
    JavascriptExecutionEngine, PythonExecutionEngine, StartJavascriptExecutionRequest,
    StartPythonExecutionRequest, StartWasmExecutionRequest, WasmExecutionEngine,
    WasmPermissionTier,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

const ARG_PREFIX: &str = "ARG=";
const ENV_PREFIX: &str = "ENV=";
const INVOCATION_BREAK: &str = "--END--";
const NODE_ALLOW_CHILD_PROCESS_FLAG: &str = "--allow-child-process";
const NODE_ALLOW_WORKER_FLAG: &str = "--allow-worker";
const NODE_ALLOW_WASI_FLAG: &str = "--allow-wasi";
const NODE_ALLOW_FS_READ_FLAG: &str = "--allow-fs-read=";
const NODE_ALLOW_FS_WRITE_FLAG: &str = "--allow-fs-write=";
const NODE_MAX_OLD_SPACE_SIZE_FLAG_PREFIX: &str = "--max-old-space-size=";
const NODE_STACK_SIZE_FLAG_PREFIX: &str = "--stack-size=";
const NODE_WASM_MAX_MEM_PAGES_FLAG_PREFIX: &str = "--wasm-max-mem-pages=";
const PYTHON_MAX_OLD_SPACE_MB_ENV: &str = "AGENT_OS_PYTHON_MAX_OLD_SPACE_MB";
const WASM_MAX_FUEL_ENV: &str = "AGENT_OS_WASM_MAX_FUEL";
const WASM_MAX_MEMORY_BYTES_ENV: &str = "AGENT_OS_WASM_MAX_MEMORY_BYTES";
const JAVASCRIPT_TEST_VM_ID: &str = "vm-js";
const PYTHON_TEST_VM_ID: &str = "vm-python";
const WASM_TEST_VM_ID: &str = "vm-wasm";
static NEXT_TEST_IMPORT_CACHE_ID: AtomicUsize = AtomicUsize::new(0);
static NODE_BINARY_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

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

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn node_binary_env_guard() -> MutexGuard<'static, ()> {
    NODE_BINARY_ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("lock node binary env guard")
}

fn next_import_cache_base_dir(prefix: &str) -> PathBuf {
    let cache_id = NEXT_TEST_IMPORT_CACHE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "agent-os-node-import-cache-{prefix}-{}-{cache_id}",
        std::process::id()
    ))
}

fn new_javascript_test_engine() -> JavascriptExecutionEngine {
    let mut engine = JavascriptExecutionEngine::default();
    engine.set_import_cache_base_dir(
        JAVASCRIPT_TEST_VM_ID,
        next_import_cache_base_dir("permission-js"),
    );
    engine
}

fn new_python_test_engine() -> PythonExecutionEngine {
    let mut engine = PythonExecutionEngine::default();
    engine.set_import_cache_base_dir(
        PYTHON_TEST_VM_ID,
        next_import_cache_base_dir("permission-python"),
    );
    engine
}

fn new_wasm_test_engine() -> WasmExecutionEngine {
    let mut engine = WasmExecutionEngine::default();
    engine.set_import_cache_base_dir(
        WASM_TEST_VM_ID,
        next_import_cache_base_dir("permission-wasm"),
    );
    engine
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|error| panic!("canonicalize {}: {error}", path.display()))
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

fn read_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_READ_FLAG))
        .collect()
}

fn write_flags(args: &[String]) -> Vec<&str> {
    args.iter()
        .filter_map(|arg| arg.strip_prefix(NODE_ALLOW_FS_WRITE_FLAG))
        .collect()
}

#[test]
fn node_permission_flags_do_not_expose_workspace_root_or_entrypoint_parent_writes() {
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    let js_entry_dir = js_cwd.join("nested");
    fs::create_dir_all(&js_entry_dir).expect("create js entry dir");
    fs::write(js_entry_dir.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = new_javascript_test_engine();
    let js_context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
        bootstrap_module: None,
        compile_cache_root: None,
    });
    let js_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
            context_id: js_context.context_id,
            argv: vec![String::from("./nested/entry.mjs")],
            env: BTreeMap::new(),
            cwd: js_cwd.clone(),
        })
        .expect("start javascript execution")
        .wait()
        .expect("wait for javascript execution");
    assert_eq!(js_result.exit_code, 0);

    let wasm_cwd = temp.path().join("wasm-project");
    let wasm_module_dir = wasm_cwd.join("modules");
    fs::create_dir_all(&wasm_module_dir).expect("create wasm module dir");
    fs::write(wasm_module_dir.join("guest.wasm"), []).expect("write wasm module");

    let pyodide_dir = temp.path().join("pyodide-dist");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dist dir");
    fs::write(
        pyodide_dir.join("pyodide.mjs"),
        "export async function loadPyodide() { return { async runPythonAsync() {} }; }\n",
    )
    .expect("write pyodide fixture");
    fs::write(pyodide_dir.join("pyodide-lock.json"), "{\"packages\":[]}\n")
        .expect("write pyodide lock fixture");

    let mut python_engine = new_python_test_engine();
    let python_context = python_engine.create_context(CreatePythonContextRequest {
        vm_id: String::from(PYTHON_TEST_VM_ID),
        pyodide_dist_path: pyodide_dir.clone(),
    });
    let python_result = python_engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from(PYTHON_TEST_VM_ID),
            context_id: python_context.context_id,
            code: String::from("print('ignored')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start python execution")
        .wait(None)
        .expect("wait for python execution");
    assert_eq!(python_result.exit_code, 0);

    let mut wasm_engine = new_wasm_test_engine();
    let wasm_context = wasm_engine.create_context(CreateWasmContextRequest {
        vm_id: String::from(WASM_TEST_VM_ID),
        module_path: Some(String::from("./modules/guest.wasm")),
    });
    let wasm_result = wasm_engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from(WASM_TEST_VM_ID),
            context_id: wasm_context.context_id,
            argv: vec![String::from("./modules/guest.wasm")],
            env: BTreeMap::from([(
                String::from(WASM_MAX_STACK_BYTES_ENV),
                String::from("131072"),
            )]),
            cwd: wasm_cwd.clone(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution")
        .wait()
        .expect("wait for wasm execution");
    assert_eq!(wasm_result.exit_code, 0);

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        5,
        "expected javascript exec plus python prewarm and exec plus wasm prewarm and exec"
    );

    let workspace_root = canonical(&workspace_root()).display().to_string();
    let js_entry_parent = canonical(&js_entry_dir).display().to_string();
    let python_cwd = canonical(temp.path()).display().to_string();
    let python_pyodide_dir = canonical(&pyodide_dir).display().to_string();
    let wasm_module_parent = canonical(&wasm_module_dir).display().to_string();

    let javascript_args = &invocations[0];
    let javascript_reads = read_flags(javascript_args);
    let javascript_writes = write_flags(javascript_args);
    assert!(
        !javascript_reads
            .iter()
            .any(|path| *path == workspace_root.as_str()),
        "javascript read flags should not include workspace root: {javascript_args:?}"
    );
    assert!(
        javascript_reads
            .iter()
            .any(|path| *path == js_entry_parent.as_str()),
        "javascript read flags should include the entrypoint parent: {javascript_args:?}"
    );
    assert!(
        !javascript_writes
            .iter()
            .any(|path| *path == js_entry_parent.as_str()),
        "javascript write flags should not include the entrypoint parent: {javascript_args:?}"
    );

    for python_args in &invocations[1..3] {
        let python_reads = read_flags(python_args);
        let python_writes = write_flags(python_args);
        assert!(
            python_args.iter().any(|arg| arg == "--permission"),
            "python should run under Node permission mode: {python_args:?}"
        );
        assert!(
            python_reads.iter().any(|path| *path == python_cwd.as_str()),
            "python should receive fs read access for the sandbox cwd: {python_args:?}"
        );
        assert!(
            python_reads
                .iter()
                .any(|path| *path == python_pyodide_dir.as_str()),
            "python should receive fs read access for the Pyodide bundle: {python_args:?}"
        );
        assert!(
            python_reads
                .iter()
                .any(|path| path.contains("agent-os-node-import-cache-")),
            "python should receive fs read access for the shared import cache: {python_args:?}"
        );
        assert!(
            python_writes
                .iter()
                .any(|path| *path == python_cwd.as_str()),
            "python should receive fs write access for the sandbox cwd: {python_args:?}"
        );
        assert!(
            python_writes
                .iter()
                .any(|path| path.contains("agent-os-node-import-cache-")),
            "python should receive fs write access for the shared import cache: {python_args:?}"
        );
        assert!(
            !python_writes
                .iter()
                .any(|path| *path == python_pyodide_dir.as_str()),
            "python should not receive fs write access for the readonly Pyodide bundle: {python_args:?}"
        );
    }

    for wasm_args in &invocations[3..] {
        let wasm_reads = read_flags(wasm_args);
        let wasm_writes = write_flags(wasm_args);
        assert!(
            !wasm_reads
                .iter()
                .any(|path| *path == workspace_root.as_str()),
            "wasm read flags should not include workspace root: {wasm_args:?}"
        );
        assert!(
            wasm_reads
                .iter()
                .any(|path| *path == wasm_module_parent.as_str()),
            "wasm read flags should include the module parent: {wasm_args:?}"
        );
        assert!(
            !wasm_writes
                .iter()
                .any(|path| *path == wasm_module_parent.as_str()),
            "wasm write flags should not include the module parent: {wasm_args:?}"
        );
        assert!(
            wasm_args
                .iter()
                .any(|arg| arg.starts_with(NODE_STACK_SIZE_FLAG_PREFIX)),
            "wasm execution should apply the configured Node stack-size flag: {wasm_args:?}"
        );
    }
}

#[test]
fn node_permission_flags_allow_workers_for_internal_javascript_loader_runtime() {
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    fs::create_dir_all(&js_cwd).expect("create js cwd");
    fs::write(js_cwd.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = new_javascript_test_engine();
    let context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
        bootstrap_module: None,
        compile_cache_root: None,
    });

    let default_result = js_engine
        .start_execution(StartJavascriptExecutionRequest {
            vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
            context_id: context.context_id.clone(),
            argv: vec![String::from("./entry.mjs")],
            env: BTreeMap::new(),
            cwd: js_cwd.clone(),
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
        })
        .expect("start javascript execution with workers")
        .wait()
        .expect("wait for javascript execution with workers");
    assert_eq!(worker_result.exit_code, 0);

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        2,
        "expected one invocation per javascript execution"
    );
    assert!(
        invocations[0]
            .iter()
            .any(|arg| arg == NODE_ALLOW_WORKER_FLAG),
        "javascript executions should allow internal loader workers even by default: {:?}",
        invocations[0]
    );
    assert!(
        invocations[1]
            .iter()
            .any(|arg| arg == NODE_ALLOW_WORKER_FLAG),
        "javascript executions should keep worker permission enabled when worker_threads is allowed: {:?}",
        invocations[1]
    );
}

#[test]
fn node_permission_flags_only_propagate_nested_child_capabilities_when_parent_explicitly_allows_them(
) {
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let js_cwd = temp.path().join("js-project");
    fs::create_dir_all(&js_cwd).expect("create js cwd");
    fs::write(js_cwd.join("entry.mjs"), "console.log('ignored');").expect("write js entry");

    let mut js_engine = new_javascript_test_engine();
    let context = js_engine.create_context(CreateJavascriptContextRequest {
        vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
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
            vm_id: String::from(JAVASCRIPT_TEST_VM_ID),
            context_id: context.context_id.clone(),
            argv: vec![String::from("./entry.mjs")],
            env: nested_env("0", "0"),
            cwd: js_cwd.clone(),
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
        })
        .expect("start nested javascript execution with inherited permissions")
        .wait()
        .expect("wait for nested javascript execution with inherited permissions");
    assert_eq!(allowed_result.exit_code, 0);

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        2,
        "expected one invocation per nested javascript execution"
    );
    assert!(
        !invocations[0]
            .iter()
            .any(|arg| arg == NODE_ALLOW_CHILD_PROCESS_FLAG),
        "nested child should not inherit --allow-child-process without explicit parent permission: {:?}",
        invocations[0]
    );
    assert!(
        !invocations[0]
            .iter()
            .any(|arg| arg == NODE_ALLOW_WORKER_FLAG),
        "nested child should not inherit --allow-worker without explicit parent permission: {:?}",
        invocations[0]
    );
    assert!(
        invocations[1]
            .iter()
            .any(|arg| arg == NODE_ALLOW_CHILD_PROCESS_FLAG),
        "nested child should preserve --allow-child-process when the parent explicitly had it: {:?}",
        invocations[1]
    );
    assert!(
        invocations[1]
            .iter()
            .any(|arg| arg == NODE_ALLOW_WORKER_FLAG),
        "nested child should preserve --allow-worker when the parent explicitly had it: {:?}",
        invocations[1]
    );
}

#[test]
fn python_execution_applies_configured_heap_limit_to_prewarm_and_exec_processes() {
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let pyodide_dir = temp.path().join("pyodide-dist");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dist dir");
    fs::write(
        pyodide_dir.join("pyodide.mjs"),
        "export async function loadPyodide() { return { async runPythonAsync() {} }; }\n",
    )
    .expect("write pyodide fixture");
    fs::write(pyodide_dir.join("pyodide-lock.json"), "{\"packages\":[]}\n")
        .expect("write pyodide lock fixture");

    let mut python_engine = new_python_test_engine();
    let context = python_engine.create_context(CreatePythonContextRequest {
        vm_id: String::from(PYTHON_TEST_VM_ID),
        pyodide_dist_path: pyodide_dir,
    });

    let result = python_engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from(PYTHON_TEST_VM_ID),
            context_id: context.context_id,
            code: String::from("print('ignored')"),
            file_path: None,
            env: BTreeMap::from([(
                String::from(PYTHON_MAX_OLD_SPACE_MB_ENV),
                String::from("256"),
            )]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start python execution")
        .wait(None)
        .expect("wait for python execution");
    assert_eq!(result.exit_code, 0);

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        2,
        "expected one prewarm invocation and one execution invocation"
    );

    for args in &invocations {
        assert!(
            args.iter()
                .any(|arg| arg == &format!("{NODE_MAX_OLD_SPACE_SIZE_FLAG_PREFIX}256")),
            "python invocations should apply the configured Node heap limit: {args:?}"
        );
    }
}

#[test]
fn wasm_execution_passes_runtime_memory_and_fuel_limits_to_node_process() {
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let wasm_cwd = temp.path().join("wasm-project");
    fs::create_dir_all(&wasm_cwd).expect("create wasm cwd");
    fs::write(wasm_cwd.join("guest.wasm"), b"\0asm\x01\0\0\0").expect("write wasm module");

    let mut engine = new_wasm_test_engine();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from(WASM_TEST_VM_ID),
        module_path: Some(String::from("./guest.wasm")),
    });

    let result = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from(WASM_TEST_VM_ID),
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
    let _env_lock = node_binary_env_guard();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    let mut engine = new_wasm_test_engine();
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
            vm_id: String::from(WASM_TEST_VM_ID),
            module_path: Some(String::from("./guest.wasm")),
        });

        let result = engine
            .start_execution(StartWasmExecutionRequest {
                vm_id: String::from(WASM_TEST_VM_ID),
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
