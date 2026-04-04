use agent_os_execution::{
    CreatePythonContextRequest, PythonExecutionEngine, StartPythonExecutionRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tempfile::tempdir;

const ARG_PREFIX: &str = "ARG=";
const ENV_NODE_COMPILE_CACHE_PREFIX: &str = "ENV_NODE_COMPILE_CACHE=";
const ENV_PREWARM_PREFIX: &str = "ENV_PREWARM=";
const INVOCATION_BREAK: &str = "--END--";

#[derive(Debug, Clone, PartialEq, Eq)]
struct LoggedInvocation {
    args: Vec<String>,
    node_compile_cache: String,
    prewarm_only: bool,
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

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

fn write_fixture(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fixture");
}

fn write_pyodide_lock_fixture(path: &Path) {
    write_fixture(path, "{\"packages\":[]}\n");
}

fn write_fake_node_binary(path: &Path, log_path: &Path) {
    let script = format!(
        "#!/bin/sh\nset -eu\nlog=\"{}\"\ncompile_cache=\"${{NODE_COMPILE_CACHE:-}}\"\nif [ -n \"$compile_cache\" ]; then\n  mkdir -p \"$compile_cache\"\n  touch \"$compile_cache/fake-compiled-${{AGENT_OS_PYTHON_PREWARM_ONLY:-exec}}\"\nfi\nfor arg in \"$@\"; do\n  printf 'ARG=%s\\n' \"$arg\" >> \"$log\"\ndone\nprintf 'ENV_NODE_COMPILE_CACHE=%s\\n' \"$compile_cache\" >> \"$log\"\nprintf 'ENV_PREWARM=%s\\n' \"${{AGENT_OS_PYTHON_PREWARM_ONLY:-0}}\" >> \"$log\"\nprintf '%s\\n' '{}' >> \"$log\"\nexit 0\n",
        log_path.display(),
        INVOCATION_BREAK,
    );
    fs::write(path, script).expect("write fake node binary");
    let mut permissions = fs::metadata(path)
        .expect("fake node metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
}

fn parse_invocations(log_path: &Path) -> Vec<LoggedInvocation> {
    let contents = fs::read_to_string(log_path).expect("read invocation log");
    let separator = format!("{INVOCATION_BREAK}\n");

    contents
        .split(&separator)
        .filter(|block| !block.trim().is_empty())
        .map(|block| {
            let args = block
                .lines()
                .filter_map(|line| line.strip_prefix(ARG_PREFIX))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let node_compile_cache = block
                .lines()
                .find_map(|line| line.strip_prefix(ENV_NODE_COMPILE_CACHE_PREFIX))
                .unwrap_or_default()
                .to_owned();
            let prewarm_only = block
                .lines()
                .find_map(|line| line.strip_prefix(ENV_PREWARM_PREFIX))
                .is_some_and(|value| value == "1");

            LoggedInvocation {
                args,
                node_compile_cache,
                prewarm_only,
            }
        })
        .collect()
}

fn start_python_execution(
    engine: &mut PythonExecutionEngine,
    context_id: String,
    cwd: &Path,
    code: &str,
) {
    let result = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id,
            code: String::from(code),
            file_path: None,
            env: BTreeMap::new(),
            cwd: cwd.to_path_buf(),
        })
        .expect("start Python execution")
        .wait()
        .expect("wait for Python execution");
    assert_eq!(result.exit_code, 0);
}

fn setup_engine(pyodide_dir: PathBuf) -> (PythonExecutionEngine, String) {
    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });
    (engine, context.context_id)
}

#[test]
fn python_execution_prewarms_once_when_compile_cache_is_ready() {
    let _lock = env_lock().lock().expect("lock env mutation");
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set_path("AGENT_OS_NODE_BINARY", &fake_node_path);

    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        "export async function loadPyodide() { return { async runPythonAsync() {} }; }\n",
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let (mut engine, context_id) = setup_engine(pyodide_dir);
    start_python_execution(
        &mut engine,
        context_id.clone(),
        temp.path(),
        "print('first')",
    );
    start_python_execution(&mut engine, context_id, temp.path(), "print('second')");

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        3,
        "expected prewarm plus first exec plus second exec: {invocations:?}"
    );
    assert!(
        invocations[0].prewarm_only,
        "first invocation should prewarm"
    );
    assert!(
        !invocations[1].prewarm_only,
        "second invocation should execute"
    );
    assert!(
        !invocations[2].prewarm_only,
        "third invocation should execute"
    );

    for invocation in &invocations {
        assert!(
            invocation
                .args
                .iter()
                .any(|arg| arg.contains("python-runner.mjs")),
            "expected python runner invocation: {invocation:?}"
        );
        assert!(
            !invocation.node_compile_cache.is_empty(),
            "expected NODE_COMPILE_CACHE for Python prewarm and exec: {invocation:?}"
        );
    }

    let compile_cache_dir = Path::new(&invocations[0].node_compile_cache);
    assert!(
        compile_cache_dir.join("fake-compiled-1").is_file(),
        "expected prewarm to populate compile cache"
    );
}

#[test]
fn python_execution_invalidates_prewarm_stamp_when_pyodide_bundle_changes() {
    let _lock = env_lock().lock().expect("lock env mutation");
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-args.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set_path("AGENT_OS_NODE_BINARY", &fake_node_path);

    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    let pyodide_mjs = pyodide_dir.join("pyodide.mjs");
    write_fixture(
        &pyodide_mjs,
        "export async function loadPyodide() { return { async runPythonAsync() {} }; }\n",
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let (mut engine, context_id) = setup_engine(pyodide_dir);
    start_python_execution(
        &mut engine,
        context_id.clone(),
        temp.path(),
        "print('first')",
    );

    std::thread::sleep(Duration::from_millis(5));
    write_fixture(
        &pyodide_mjs,
        "export async function loadPyodide() { return { async runPythonAsync() { return 'v2'; } }; }\n",
    );

    start_python_execution(&mut engine, context_id, temp.path(), "print('second')");

    let invocations = parse_invocations(&log_path);
    assert_eq!(
        invocations.len(),
        4,
        "expected prewarm + exec twice after bundle change: {invocations:?}"
    );
    assert!(
        invocations[0].prewarm_only,
        "first invocation should prewarm"
    );
    assert!(
        !invocations[1].prewarm_only,
        "second invocation should execute"
    );
    assert!(
        invocations[2].prewarm_only,
        "third invocation should re-prewarm"
    );
    assert!(
        !invocations[3].prewarm_only,
        "fourth invocation should execute"
    );
}
