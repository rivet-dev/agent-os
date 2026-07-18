mod support;

use agentos_execution::{
    CreatePythonContextRequest, PythonExecutionEngine, PythonExecutionEvent,
    StartPythonExecutionRequest,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::tempdir;

const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENTOS_PYTHON_WARMUP_METRICS__:";

fn setup_engine() -> (PythonExecutionEngine, String, PathBuf) {
    let mut engine = support::python_engine();
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
    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            guest_runtime: Default::default(),
            limits: Default::default(),
            vm_id: String::from("vm-python"),
            context_id: context_id.to_string(),
            code: code.to_string(),
            file_path: None,
            env: BTreeMap::from([(
                String::from("AGENTOS_PYTHON_WARMUP_DEBUG"),
                String::from("1"),
            )]),
            cwd: cwd.to_path_buf(),
        })
        .expect("start Python execution");

    // Drive the event loop directly instead of `.wait()`: the Pyodide runner
    // now sets up a kernel-VFS-backed site-packages on boot, which emits VFS
    // RPCs. This prewarm test has no VFS backend, so reject those RPCs and let
    // the runner's best-effort site-packages setup degrade. Module-resolution
    // sync RPCs are serviced host-directly.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        match execution
            .poll_event_blocking(Duration::from_secs(60))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(PythonExecutionEvent::HostRpcRequest(request)) => {
                let serviced = execution
                    .try_service_standalone_module_sync_rpc(&request)
                    .expect("service module sync RPC");
                assert!(serviced, "unexpected JS sync RPC request: {request:?}");
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                execution
                    .respond_vfs_rpc_error(request.id, "ENOSYS", "no VFS backend in prewarm test")
                    .expect("respond to VFS RPC");
            }
            Some(PythonExecutionEvent::Exited(exit_code)) => {
                return (
                    String::from_utf8(stdout).expect("stdout utf8"),
                    String::from_utf8(stderr).expect("stderr utf8"),
                    exit_code,
                );
            }
            None => panic!("timed out waiting for Python execution event"),
        }
    }
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
//
// cargo-nextest runs every `#[test]` in its OWN process, so that crash cannot
// occur there — `mod python_prewarm_split` below exposes each case as a separate
// test that nextest runs in parallel across cores. Skip the collapsed run under
// nextest so the work isn't done twice; the split cases skip themselves under
// `cargo test`. nextest sets `NEXTEST=1` in each test process; libtest/`cargo
// test` does not.
#[test]
fn python_prewarm_suite() {
    if std::env::var_os("NEXTEST").is_some() {
        return;
    }
    python_execution_prewarms_once_when_compile_cache_is_ready();
    python_execution_invalidates_prewarm_stamp_when_pyodide_bundle_changes();
}

/// Per-case split of `python_prewarm_suite` for cargo-nextest (process-per-test).
///
/// Each `#[test]` here runs exactly one Pyodide prewarm case in its own process,
/// so the shared-process V8/Pyodide teardown/init crash that forces the collapsed
/// `python_prewarm_suite` above does not apply, and the cases run in parallel
/// across cores instead of serially. Each case skips itself under plain `cargo
/// test` (where `NEXTEST` is unset) so the collapsed suite owns the run there; the
/// collapsed suite likewise skips under nextest so the work isn't duplicated.
mod python_prewarm_split {
    macro_rules! nextest_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[test]
                fn $case() {
                    // Covered by the collapsed `super::python_prewarm_suite` under `cargo test`.
                    if std::env::var_os("NEXTEST").is_none() {
                        return;
                    }
                    super::$case();
                }
            )+
        };
    }

    nextest_cases!(
        python_execution_prewarms_once_when_compile_cache_is_ready,
        python_execution_invalidates_prewarm_stamp_when_pyodide_bundle_changes,
    );
}
