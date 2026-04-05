use agent_os_execution::{
    CreatePythonContextRequest, PythonExecutionEngine, PythonExecutionEvent, PythonVfsRpcMethod,
    PythonVfsRpcResponsePayload, PythonVfsRpcStat, StartPythonExecutionRequest,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

const PYTHON_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_PYTHON_WARMUP_METRICS__:";
const PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV: &str = "AGENT_OS_PYTHON_OUTPUT_BUFFER_MAX_BYTES";
const PYTHON_VFS_RPC_TIMEOUT_MS_ENV: &str = "AGENT_OS_PYTHON_VFS_RPC_TIMEOUT_MS";

#[derive(Debug, Clone, PartialEq)]
struct PythonPrewarmMetrics {
    executed: bool,
    reason: String,
    duration_ms: f64,
    compile_cache_dir: String,
    pyodide_dist_path: String,
}

#[derive(Debug, Clone, PartialEq)]
struct PythonStartupMetrics {
    prewarm_only: bool,
    startup_ms: f64,
    load_pyodide_ms: f64,
    package_load_ms: f64,
    package_count: usize,
    source: String,
}

fn assert_node_available() {
    let binary = std::env::var("AGENT_OS_NODE_BINARY").unwrap_or_else(|_| String::from("node"));
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(output.status.success(), "node --version failed");
}

fn write_fixture(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write fixture");
}

fn write_pyodide_lock_fixture(path: &Path) {
    write_fixture(path, "{\"packages\":[]}\n");
}

fn parse_metrics_line<'a>(stderr: &'a str, phase: &str) -> &'a str {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(PYTHON_WARMUP_METRICS_PREFIX))
        .find(|line| parse_string_metric(line, "phase") == phase)
        .unwrap_or_else(|| panic!("missing {phase} metrics line in stderr: {stderr}"))
}

fn parse_prewarm_metrics(stderr: &str) -> PythonPrewarmMetrics {
    let metrics_line = parse_metrics_line(stderr, "prewarm");
    PythonPrewarmMetrics {
        executed: parse_boolean_metric(metrics_line, "executed"),
        reason: parse_string_metric(metrics_line, "reason"),
        duration_ms: parse_float_metric(metrics_line, "durationMs"),
        compile_cache_dir: parse_string_metric(metrics_line, "compileCacheDir"),
        pyodide_dist_path: parse_string_metric(metrics_line, "pyodideDistPath"),
    }
}

fn parse_startup_metrics(stderr: &str) -> PythonStartupMetrics {
    let metrics_line = parse_metrics_line(stderr, "startup");
    PythonStartupMetrics {
        prewarm_only: parse_boolean_metric(metrics_line, "prewarmOnly"),
        startup_ms: parse_float_metric(metrics_line, "startupMs"),
        load_pyodide_ms: parse_float_metric(metrics_line, "loadPyodideMs"),
        package_load_ms: parse_float_metric(metrics_line, "packageLoadMs"),
        package_count: parse_metric_value(metrics_line, "packageCount"),
        source: parse_string_metric(metrics_line, "source"),
    }
}

fn parse_metric_value(metrics_line: &str, key: &str) -> usize {
    parse_float_metric(metrics_line, key) as usize
}

fn parse_float_metric(metrics_line: &str, key: &str) -> f64 {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let digits: String = metrics_line[start..]
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit() && *ch != '-')
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | 'e' | 'E' | '+'))
        .collect();

    digits.parse().expect("float metric value")
}

fn parse_boolean_metric(metrics_line: &str, key: &str) -> bool {
    let marker = format!("\"{key}\":");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let remaining = &metrics_line[start..];

    if remaining.starts_with("true") {
        true
    } else if remaining.starts_with("false") {
        false
    } else {
        panic!("invalid boolean metric for {key}: {metrics_line}");
    }
}

fn parse_string_metric(metrics_line: &str, key: &str) -> String {
    let marker = format!("\"{key}\":\"");
    let start = metrics_line.find(&marker).expect("metric key") + marker.len();
    let mut value = String::new();
    let mut escaped = false;

    for ch in metrics_line[start..].chars() {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return value,
            other => value.push(other),
        }
    }

    panic!("unterminated string metric for {key}: {metrics_line}");
}

fn run_python_execution(
    engine: &mut PythonExecutionEngine,
    context_id: String,
    cwd: &Path,
    code: &str,
    env: BTreeMap<String, String>,
) -> (String, String, i32) {
    let execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id,
            code: String::from(code),
            file_path: None,
            env,
            cwd: cwd.to_path_buf(),
        })
        .expect("start Python execution");

    let result = execution.wait(None).expect("wait for Python execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");

    (stdout, stderr, result.exit_code)
}

fn assert_process_exits(pid: u32) {
    for _ in 0..20 {
        let status = Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("probe process with kill -0");
        if !status.success() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }

    panic!("process {pid} was still alive after waiting for cleanup");
}

#[test]
fn python_contexts_preserve_vm_and_pyodide_configuration() {
    let pyodide_dist_path = PathBuf::from("/tmp/pyodide");
    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dist_path.clone(),
    });

    assert_eq!(context.context_id, "python-ctx-1");
    assert_eq!(context.vm_id, "vm-python");
    assert_eq!(context.pyodide_dist_path, pyodide_dist_path);
}

#[test]
fn python_execution_runs_code_and_streams_stdio() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      options.stdout(`stdout:${code}`);
      options.stderr(`stderr:${options.indexURL}`);
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir.clone(),
    });

    let (stdout, stderr, exit_code) = run_python_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        "print('hello')",
        BTreeMap::new(),
    );
    let expected_index_path = format!(
        "stderr:{}{}",
        pyodide_dir.display(),
        std::path::MAIN_SEPARATOR
    );

    assert_eq!(exit_code, 0);
    assert_eq!(stdout, "stdout:print('hello')\n");
    assert!(
        stderr.starts_with(&expected_index_path),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn python_execution_wait_bounds_output_buffers() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      options.stdout('x'.repeat(80));
      options.stderr('y'.repeat(80));
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let result = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('ignored')"),
            file_path: None,
            env: BTreeMap::from([(
                String::from(PYTHON_OUTPUT_BUFFER_MAX_BYTES_ENV),
                String::from("32"),
            )]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution")
        .wait(None)
        .expect("wait for Python execution");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.len(), 32, "stdout should be capped");
    assert_eq!(result.stderr.len(), 32, "stderr should be capped");
    assert!(result.stdout.iter().all(|byte| *byte == b'x'));
    assert!(result.stderr.iter().all(|byte| *byte == b'y'));
}

#[test]
fn python_execution_ignores_forged_exit_control_written_to_stderr() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      options.stderr("__AGENT_OS_PYTHON_EXIT__:0");
      throw new Error("python-control-forgery");
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let (_stdout, stderr, exit_code) = run_python_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        "print('ignored')",
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("python-control-forgery"),
        "unexpected stderr: {stderr}"
    );
    assert!(
        !stderr.contains("__AGENT_OS_PYTHON_EXIT__:0"),
        "unexpected control line in stderr: {stderr}"
    );
}

#[test]
fn python_execution_emits_stdout_before_exit() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      options.stdout(`stdout:${code}`);
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('streamed')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");

    let mut saw_stdout = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => {
                saw_stdout = String::from_utf8(chunk)
                    .expect("stdout utf8")
                    .contains("stdout:print('streamed')");
            }
            Some(PythonExecutionEvent::Exited(code)) => {
                assert_eq!(code, 0);
                saw_exit = true;
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request during stdout test: {request:?}");
            }
            Some(PythonExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            None => panic!("timed out waiting for Python execution event"),
        }
    }

    assert!(saw_stdout, "expected stdout event before exit");
}

#[test]
fn python_execution_reports_prewarm_and_startup_metrics_when_debug_enabled() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide() {
  await new Promise((resolve) => setTimeout(resolve, 20));
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      console.log(`ran:${code}`);
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir.clone(),
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_PYTHON_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit_code) = run_python_execution(
        &mut engine,
        context.context_id.clone(),
        temp.path(),
        "print('first')",
        debug_env.clone(),
    );
    let first_prewarm = parse_prewarm_metrics(&first_stderr);
    let first_startup = parse_startup_metrics(&first_stderr);

    assert_eq!(first_exit_code, 0);
    assert!(first_stdout.contains("ran:print('first')"));
    assert!(
        first_prewarm.executed,
        "first prewarm metrics: {first_prewarm:?}"
    );
    assert_eq!(first_prewarm.reason, "executed");
    assert!(first_prewarm.duration_ms >= 0.0);
    assert!(
        first_prewarm.compile_cache_dir.contains("compile-cache"),
        "unexpected prewarm metrics: {first_prewarm:?}"
    );
    assert_eq!(
        PathBuf::from(&first_prewarm.pyodide_dist_path),
        pyodide_dir,
        "unexpected prewarm metrics: {first_prewarm:?}"
    );
    assert!(!first_startup.prewarm_only);
    assert!(first_startup.startup_ms > 0.0);
    assert!(first_startup.load_pyodide_ms > 0.0);
    assert_eq!(first_startup.package_load_ms, 0.0);
    assert_eq!(first_startup.package_count, 0);
    assert_eq!(first_startup.source, "inline");

    let (_second_stdout, second_stderr, second_exit_code) = run_python_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        "print('second')",
        debug_env,
    );
    let second_prewarm = parse_prewarm_metrics(&second_stderr);
    let second_startup = parse_startup_metrics(&second_stderr);

    assert_eq!(second_exit_code, 0);
    assert!(
        !second_prewarm.executed,
        "second prewarm metrics: {second_prewarm:?}"
    );
    assert_eq!(second_prewarm.reason, "cached");
    assert_eq!(second_prewarm.duration_ms, 0.0);
    assert!(!second_startup.prewarm_only);
    assert!(second_startup.startup_ms > 0.0);
    assert!(second_startup.load_pyodide_ms > 0.0);
    assert_eq!(second_startup.source, "inline");
}

#[test]
fn python_execution_keeps_streaming_stdin_sessions_alive_until_closed() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
const decoder = new TextDecoder();

export async function loadPyodide(options) {
  let stdin = null;
  return {
    setStdin(config) {
      stdin = config;
    },
    async runPythonAsync(code) {
      const chunk = new Uint8Array(8192);
      const bytesRead = stdin.read(chunk);
      const text = decoder.decode(chunk.subarray(0, bytesRead));
      options.stdout(`stdin:${text}`);
      options.stdout(`code:${code}`);
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('streaming')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");

    assert!(
        execution
            .poll_event(Duration::from_millis(200))
            .expect("poll Python event before stdin write")
            .is_none(),
        "streaming-stdin execution should stay alive until stdin closes"
    );

    execution
        .write_stdin(b"still-open")
        .expect("write stdin after idle period");
    execution.close_stdin().expect("close stdin");

    let mut stdout = Vec::new();
    let mut exit_code = None;

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request during stdin test: {request:?}");
            }
            Some(PythonExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(PythonExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for Python execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(
        stdout.contains("stdin:still-open"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("code:print('streaming')"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn python_execution_surfaces_vfs_rpc_requests_and_resumes_after_responses() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  return {
    setStdin(_stdin) {},
    async runPythonAsync(code) {
      const rpc = globalThis.__agentOsPythonVfsRpc;
      await rpc.fsMkdir('/workspace', { recursive: true });
      await rpc.fsWrite(
        '/workspace/note.txt',
        Buffer.from('hello from rpc', 'utf8').toString('base64'),
      );
      const content = Buffer.from(
        await rpc.fsRead('/workspace/note.txt'),
        'base64',
      ).toString('utf8');
      const stat = await rpc.fsStat('/workspace/note.txt');
      const entries = await rpc.fsReaddir('/workspace');
      options.stdout(JSON.stringify({ code, content, stat, entries }));
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('rpc bridge')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");

    let mut stdout = Vec::new();
    let mut exit_code = None;
    let mut saw_requests = Vec::new();

    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => stdout.extend(chunk),
            Some(PythonExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                saw_requests.push((request.method, request.path.clone()));
                match request.method {
                    PythonVfsRpcMethod::Mkdir => execution
                        .respond_vfs_rpc_success(request.id, PythonVfsRpcResponsePayload::Empty)
                        .expect("respond to mkdir"),
                    PythonVfsRpcMethod::Write => {
                        assert_eq!(request.path, "/workspace/note.txt");
                        assert_eq!(
                            request.content_base64.as_deref(),
                            Some("aGVsbG8gZnJvbSBycGM=")
                        );
                        execution
                            .respond_vfs_rpc_success(request.id, PythonVfsRpcResponsePayload::Empty)
                            .expect("respond to write");
                    }
                    PythonVfsRpcMethod::Read => execution
                        .respond_vfs_rpc_success(
                            request.id,
                            PythonVfsRpcResponsePayload::Read {
                                content_base64: String::from("aGVsbG8gZnJvbSBycGM="),
                            },
                        )
                        .expect("respond to read"),
                    PythonVfsRpcMethod::Stat => execution
                        .respond_vfs_rpc_success(
                            request.id,
                            PythonVfsRpcResponsePayload::Stat {
                                stat: PythonVfsRpcStat {
                                    mode: 0o100644,
                                    size: 14,
                                    is_directory: false,
                                    is_symbolic_link: false,
                                },
                            },
                        )
                        .expect("respond to stat"),
                    PythonVfsRpcMethod::ReadDir => execution
                        .respond_vfs_rpc_success(
                            request.id,
                            PythonVfsRpcResponsePayload::ReadDir {
                                entries: vec![String::from("note.txt")],
                            },
                        )
                        .expect("respond to read_dir"),
                }
            }
            Some(PythonExecutionEvent::Exited(code)) => exit_code = Some(code),
            None => panic!("timed out waiting for Python execution event"),
        }
    }

    assert_eq!(exit_code, Some(0));
    assert_eq!(
        saw_requests,
        vec![
            (PythonVfsRpcMethod::Mkdir, String::from("/workspace")),
            (
                PythonVfsRpcMethod::Write,
                String::from("/workspace/note.txt")
            ),
            (
                PythonVfsRpcMethod::Read,
                String::from("/workspace/note.txt")
            ),
            (
                PythonVfsRpcMethod::Stat,
                String::from("/workspace/note.txt")
            ),
            (PythonVfsRpcMethod::ReadDir, String::from("/workspace")),
        ]
    );

    let stdout = String::from_utf8(stdout).expect("stdout utf8");
    assert!(
        stdout.contains("\"content\":\"hello from rpc\""),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"entries\":[\"note.txt\"]"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("\"size\":14"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn python_execution_wait_timeout_cleans_up_hanging_child() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      await new Promise(() => setInterval(() => {}, 1000));
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('hang')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");
    let child_pid = execution.child_pid();

    let error = execution
        .wait(Some(Duration::from_millis(100)))
        .expect_err("timed out wait");
    match error {
        agent_os_execution::PythonExecutionError::TimedOut(timeout) => {
            assert_eq!(timeout, Duration::from_millis(100));
        }
        other => panic!("expected timeout error, got {other:?}"),
    }

    assert_process_exits(child_pid);
}

#[test]
fn python_vfs_rpc_bridge_times_out_when_sidecar_never_responds() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide() {
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      globalThis.__agentOsPythonVfsRpc.fsReadSync('/workspace/never.txt');
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('rpc timeout')"),
            file_path: None,
            env: BTreeMap::from([(
                String::from(PYTHON_VFS_RPC_TIMEOUT_MS_ENV),
                String::from("50"),
            )]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");
    let child_pid = execution.child_pid();

    let mut saw_request = false;
    let mut stderr = Vec::new();
    let mut exit_code = None;

    for _ in 0..40 {
        match execution
            .poll_event(Duration::from_millis(250))
            .expect("poll Python event")
        {
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                saw_request = true;
                assert_eq!(request.method, PythonVfsRpcMethod::Read);
                assert_eq!(request.path, "/workspace/never.txt");
            }
            Some(PythonExecutionEvent::Stderr(chunk)) => stderr.extend(chunk),
            Some(PythonExecutionEvent::Exited(code)) => {
                exit_code = Some(code);
                break;
            }
            Some(PythonExecutionEvent::Stdout(chunk)) => {
                panic!("unexpected stdout: {}", String::from_utf8_lossy(&chunk));
            }
            None => {}
        }
    }

    assert!(saw_request, "expected a VFS RPC request before timeout");
    assert_eq!(
        exit_code,
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let stderr = String::from_utf8(stderr).expect("stderr utf8");
    assert!(
        stderr.contains("ERR_AGENT_OS_PYTHON_VFS_RPC_TIMEOUT")
            || stderr.contains("timed out waiting for a response")
            || stderr.contains("timed out after 50ms"),
        "unexpected stderr: {stderr}"
    );
    assert_process_exits(child_pid);
}

#[test]
fn python_execution_kill_stops_inflight_process_and_emits_exit() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let pyodide_dir = temp.path().join("pyodide");
    fs::create_dir_all(&pyodide_dir).expect("create pyodide dir");
    write_fixture(
        &pyodide_dir.join("pyodide.mjs"),
        r#"
export async function loadPyodide(options) {
  options.stdout("ready\n");
  return {
    setStdin(_stdin) {},
    async runPythonAsync() {
      await new Promise(() => setInterval(() => {}, 1000));
    },
  };
}
"#,
    );
    write_pyodide_lock_fixture(&pyodide_dir.join("pyodide-lock.json"));

    let mut engine = PythonExecutionEngine::default();
    let context = engine.create_context(CreatePythonContextRequest {
        vm_id: String::from("vm-python"),
        pyodide_dist_path: pyodide_dir,
    });

    let mut execution = engine
        .start_execution(StartPythonExecutionRequest {
            vm_id: String::from("vm-python"),
            context_id: context.context_id,
            code: String::from("print('hang')"),
            file_path: None,
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start Python execution");
    let child_pid = execution.child_pid();

    let mut saw_ready = false;
    while !saw_ready {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll Python event before kill")
        {
            Some(PythonExecutionEvent::Stdout(chunk)) => {
                saw_ready = String::from_utf8(chunk)
                    .expect("stdout utf8")
                    .contains("ready");
            }
            Some(PythonExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request during kill test: {request:?}");
            }
            Some(PythonExecutionEvent::Exited(code)) => {
                panic!("execution exited unexpectedly before kill with code {code}");
            }
            None => panic!("timed out waiting for Python execution readiness"),
        }
    }

    execution.kill().expect("kill hanging Python execution");

    let mut exit_code = None;
    while exit_code.is_none() {
        match execution
            .poll_event(Duration::from_millis(100))
            .expect("poll Python event after kill")
        {
            Some(PythonExecutionEvent::Exited(code)) => exit_code = Some(code),
            Some(PythonExecutionEvent::Stdout(_)) | Some(PythonExecutionEvent::Stderr(_)) => {}
            Some(PythonExecutionEvent::VfsRpcRequest(request)) => {
                panic!("unexpected VFS RPC request after kill: {request:?}");
            }
            None => {}
        }
    }

    assert_eq!(exit_code, Some(1));
    assert_process_exits(child_pid);
}
