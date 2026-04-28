use agent_os_execution::{
    CreatePythonContextRequest, PythonExecutionEngine, StartPythonExecutionRequest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_PYTHON_WARMUP_METRICS__:";

fn setup_engine() -> (PythonExecutionEngine, String, PathBuf) {
    let mut engine = PythonExecutionEngine::default();
    let pyodide_dir = engine
        .bundled_pyodide_dist_path_for_vm("vm-python")
        .expect("materialize bundled pyodide");
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir.clone(),
    });
    (engine, context.context_id, pyodide_dir)
}

fn run_python_execution(
    engine: &mut PythonExecutionEngine,
    context_id: &str,
    cwd: &Path,
    code: &str,
) -> (String, String, i32) {
    let result = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context_id.to_string(),
            code: code.to_string(),
            file_path: None,
            env: BTreeMap::from([(
                String::from("AGENT_OS_PYTHON_WARMUP_DEBUG"),
                String::from("1"),
            )]),
            cwd: cwd.to_path_buf(),
        })
        .expect("start Python execution")
        .wait(None)
        .expect("wait for Python execution");

    (
        String::from_utf8(result.stdout).expect("stdout utf8"),
        String::from_utf8(result.stderr).expect("stderr utf8"),
        result.exit_code,
    )
}

fn parse_metrics(stderr: &str, phase: &str) -> Value {
    let payload = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(PYTHON_WARMUP_METRICS_PREFIX))
        .map(|line| serde_json::from_str::<Value>(line).expect("parse metrics json"))
        .find(|value| value.get("phase").and_then(Value::as_str) == Some(phase))
        .unwrap_or_else(|| panic!("missing {phase} metrics in stderr: {stderr}"));
    payload
}

fn python_execution_prewarms_once_when_compile_cache_is_ready() {
    let temp = tempdir().expect("create temp dir");
    let (mut engine, context_id, _pyodide_dir) = setup_engine();
    let (first_stdout, first_stderr, first_exit_code) =
        run_python_execution(&mut engine, &context_id, temp.path(), "print('first')");
    let (second_stdout, second_stderr, second_exit_code) =
        run_python_execution(&mut engine, &context_id, temp.path(), "print('second')");

    assert_eq!(first_exit_code, 0, "stderr: {first_stderr}");
    assert_eq!(second_exit_code, 0, "stderr: {second_stderr}");
    assert_eq!(first_stdout, "first\n");
    assert_eq!(second_stdout, "second\n");

    let first_prewarm = parse_metrics(&first_stderr, "prewarm");
    let second_prewarm = parse_metrics(&second_stderr, "prewarm");

    assert_eq!(first_prewarm["executed"], true);
    assert_eq!(first_prewarm["reason"], "executed");
    assert_eq!(second_prewarm["executed"], false);
    assert_eq!(second_prewarm["reason"], "cached");
}

fn python_execution_invalidates_prewarm_stamp_when_pyodide_bundle_changes() {
    let temp = tempdir().expect("create temp dir");
    let (mut engine, context_id, pyodide_dir) = setup_engine();
    let pyodide_mjs = pyodide_dir.join("pyodide.mjs");
    let (_first_stdout, first_stderr, first_exit_code) =
        run_python_execution(&mut engine, &context_id, temp.path(), "print('first')");
    assert_eq!(first_exit_code, 0, "stderr: {first_stderr}");
    assert_eq!(
        parse_metrics(&first_stderr, "prewarm")["reason"],
        "executed"
    );

    thread::sleep(Duration::from_millis(25));
    let original = fs::read_to_string(&pyodide_mjs).expect("read pyodide module");
    fs::write(
        &pyodide_mjs,
        format!("{original}\n// prewarm invalidation test\n"),
    )
    .expect("mutate pyodide module");

    let (second_stdout, second_stderr, second_exit_code) =
        run_python_execution(&mut engine, &context_id, temp.path(), "print('second')");
    assert_eq!(second_exit_code, 0, "stderr: {second_stderr}");
    assert_eq!(second_stdout, "second\n");
    assert_eq!(
        parse_metrics(&second_stderr, "prewarm")["reason"],
        "executed"
    );
}

// Separate libtest cases in this binary still trip a V8 teardown/init crash, so
// keep the prewarm coverage in one top-level suite until that boundary is fixed.
#[test]
fn python_prewarm_suite() {
    python_execution_prewarms_once_when_compile_cache_is_ready();
    python_execution_invalidates_prewarm_stamp_when_pyodide_bundle_changes();
}
