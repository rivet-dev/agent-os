use agent_os_execution::{
    CreateWasmContextRequest, StartWasmExecutionRequest, WasmExecutionEngine, WasmExecutionEvent,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_WASM_WARMUP_METRICS__:";

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmWarmupMetrics {
    executed: bool,
    reason: String,
    module_path: String,
    compile_cache_dir: String,
}

fn assert_node_available() {
    let binary = std::env::var("AGENT_OS_NODE_BINARY").unwrap_or_else(|_| String::from("node"));
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("spawn node --version");
    assert!(output.status.success(), "node --version failed");
}

fn write_fixture(path: &Path, contents: &[u8]) {
    fs::write(path, contents).expect("write fixture");
}

fn parse_warmup_metrics(stderr: &str) -> WasmWarmupMetrics {
    let metrics_line = stderr
        .lines()
        .filter_map(|line| line.strip_prefix(WASM_WARMUP_METRICS_PREFIX))
        .last()
        .expect("warmup metrics line");

    WasmWarmupMetrics {
        executed: parse_boolean_metric(metrics_line, "executed"),
        reason: parse_string_metric(metrics_line, "reason"),
        module_path: parse_string_metric(metrics_line, "modulePath"),
        compile_cache_dir: parse_string_metric(metrics_line, "compileCacheDir"),
    }
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
    let mut chars = metrics_line[start..].chars();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => value.push(parse_escaped_char(&mut chars)),
            '"' => return value,
            other => value.push(other),
        }
    }

    panic!("unterminated string metric for {key}: {metrics_line}");
}

fn parse_escaped_char(chars: &mut std::str::Chars<'_>) -> char {
    match chars.next().expect("escaped character") {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        '"' => '"',
        '\\' => '\\',
        'u' => parse_unicode_escape(chars),
        other => other,
    }
}

fn parse_unicode_escape(chars: &mut std::str::Chars<'_>) -> char {
    let high = parse_unicode_escape_unit(chars);
    if !(0xD800..=0xDBFF).contains(&high) {
        return char::from_u32(u32::from(high)).expect("basic multilingual plane char");
    }

    assert_eq!(chars.next(), Some('\\'), "expected low surrogate escape");
    assert_eq!(chars.next(), Some('u'), "expected low surrogate marker");
    let low = parse_unicode_escape_unit(chars);
    let codepoint = 0x10000 + (((u32::from(high) - 0xD800) << 10) | (u32::from(low) - 0xDC00));
    char::from_u32(codepoint).expect("supplementary plane char")
}

fn parse_unicode_escape_unit(chars: &mut std::str::Chars<'_>) -> u16 {
    let hex: String = chars.take(4).collect();
    assert_eq!(hex.len(), 4, "expected four hex digits in unicode escape");
    u16::from_str_radix(&hex, 16).expect("unicode escape value")
}

fn run_wasm_execution(
    engine: &mut WasmExecutionEngine,
    context_id: String,
    cwd: &Path,
    argv: Vec<String>,
    env: BTreeMap<String, String>,
) -> (String, String, i32) {
    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id,
            argv,
            env,
            cwd: cwd.to_path_buf(),
        })
        .expect("start wasm execution");

    let result = execution.wait().expect("wait for wasm execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");

    (stdout, stderr, result.exit_code)
}

fn wasm_stdout_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "stdout:wasm-smoke\n")
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 16))
    (i32.store (i32.const 4) (i32.const 18))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 40)
      )
    )
  )
)
"#,
    )
    .expect("compile wasm fixture")
}

fn wasm_override_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 16) "stdout:evil-smoke\n")
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 16))
    (i32.store (i32.const 4) (i32.const 18))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 40)
      )
    )
  )
)
"#,
    )
    .expect("compile wasm fixture")
}

fn wasm_timing_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $clock_time_get_t (func (param i32 i64 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "clock_time_get" (func $clock_time_get (type $clock_time_get_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 32) "timing:frozen\n")
  (func $_start (export "_start")
    (local $counter i32)
    (drop (call $clock_time_get (i32.const 0) (i64.const 1) (i32.const 0)))
    (loop $spin
      local.get $counter
      i32.const 1
      i32.add
      local.tee $counter
      i32.const 20000000
      i32.lt_u
      br_if $spin
    )
    (drop (call $clock_time_get (i32.const 0) (i64.const 1) (i32.const 8)))
    (if
      (i64.ne (i64.load (i32.const 0)) (i64.load (i32.const 8)))
      (then unreachable)
    )
    (i32.store (i32.const 16) (i32.const 32))
    (i32.store (i32.const 20) (i32.const 14))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 16)
        (i32.const 1)
        (i32.const 24)
      )
    )
  )
)
"#,
    )
    .expect("compile timing wasm fixture")
}

fn wasm_signal_state_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $proc_sigaction_t (func (param i32 i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "host_process" "proc_sigaction" (func $proc_sigaction (type $proc_sigaction_t)))
  (memory (export "memory") 1)
  (data (i32.const 32) "signal:ready\n")
  (func $_start (export "_start")
    (drop
      (call $proc_sigaction
        (i32.const 2)
        (i32.const 2)
        (i32.const 16384)
        (i32.const 0)
        (i32.const 4660)
      )
    )
    (i32.store (i32.const 0) (i32.const 32))
    (i32.store (i32.const 4) (i32.const 13))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 0)
        (i32.const 1)
        (i32.const 24)
      )
    )
  )
)
"#,
    )
    .expect("compile signal wasm fixture")
}

#[test]
fn wasm_contexts_preserve_vm_and_module_configuration() {
    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    assert_eq!(context.context_id, "wasm-ctx-1");
    assert_eq!(context.vm_id, "vm-wasm");
    assert_eq!(context.module_path.as_deref(), Some("./guest.wasm"));
}

#[test]
fn wasm_execution_runs_guest_module_through_v8() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: vec![String::from("guest.wasm")],
            env: BTreeMap::from([(String::from("IGNORED_FOR_NOW"), String::from("ok"))]),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start wasm execution");

    assert_eq!(execution.execution_id(), "exec-1");

    let result = execution.wait().expect("wait for wasm execution");
    assert_eq!(result.exit_code, 0);
    assert!(
        result.stderr.is_empty(),
        "unexpected stderr: {:?}",
        result.stderr
    );

    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    assert!(stdout.contains("stdout:wasm-smoke"));
}

#[test]
fn wasm_execution_ignores_guest_overrides_for_internal_node_env() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());
    write_fixture(&temp.path().join("evil.wasm"), &wasm_override_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let (stdout, stderr, exit_code) = run_wasm_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::from([
            (
                String::from("AGENT_OS_WASM_MODULE_PATH"),
                String::from("./evil.wasm"),
            ),
            (
                String::from("AGENT_OS_WASM_PREWARM_ONLY"),
                String::from("1"),
            ),
            (String::from("NODE_OPTIONS"), String::from("--no-warnings")),
        ]),
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "stdout:wasm-smoke\n");
    assert!(!stdout.contains("evil-smoke"));
}

#[test]
fn wasm_execution_freezes_wasi_clock_time() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_timing_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let (stdout, stderr, exit_code) = run_wasm_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("timing:frozen"), "stdout: {stdout}");
}

#[test]
fn wasm_execution_rejects_vm_mismatch() {
    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-other"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: Path::new("/tmp").to_path_buf(),
        })
        .expect_err("vm mismatch should fail");

    assert!(error
        .to_string()
        .contains("guest WebAssembly context belongs to vm vm-wasm, not vm-other"));
}

#[test]
fn wasm_execution_streams_exit_event() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start wasm execution");

    let mut saw_stdout = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll wasm event")
        {
            Some(WasmExecutionEvent::Stdout(chunk)) => {
                saw_stdout = String::from_utf8(chunk)
                    .expect("stdout utf8")
                    .contains("stdout:wasm-smoke");
            }
            Some(WasmExecutionEvent::Exited(code)) => {
                assert_eq!(code, 0);
                saw_exit = true;
            }
            Some(WasmExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            Some(WasmExecutionEvent::SignalState { .. }) => {}
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert!(saw_stdout, "expected stdout event before exit");
}

#[test]
fn wasm_execution_emits_signal_state_from_control_channel() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_signal_state_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
        })
        .expect("start wasm execution");

    let mut saw_stdout = false;
    let mut saw_signal = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event(Duration::from_secs(5))
            .expect("poll wasm event")
        {
            Some(WasmExecutionEvent::Stdout(chunk)) => {
                saw_stdout = String::from_utf8(chunk)
                    .expect("stdout utf8")
                    .contains("signal:ready");
            }
            Some(WasmExecutionEvent::SignalState {
                signal,
                registration,
            }) => {
                assert_eq!(signal, 2);
                assert_eq!(
                    registration.action,
                    agent_os_execution::wasm::WasmSignalDispositionAction::User
                );
                assert_eq!(registration.mask, vec![15]);
                assert_eq!(registration.flags, 0x1234);
                saw_signal = true;
            }
            Some(WasmExecutionEvent::Exited(code)) => {
                assert_eq!(code, 0);
                saw_exit = true;
            }
            Some(WasmExecutionEvent::Stderr(chunk)) => {
                panic!("unexpected stderr: {}", String::from_utf8_lossy(&chunk));
            }
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert!(saw_stdout, "expected stdout event before exit");
    assert!(saw_signal, "expected signal-state event before exit");
}

#[test]
fn wasm_execution_reuses_shared_warmup_path_across_contexts() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let first_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });
    let second_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });
    let debug_env = BTreeMap::from([(
        String::from("AGENT_OS_WASM_WARMUP_DEBUG"),
        String::from("1"),
    )]);

    let (first_stdout, first_stderr, first_exit) = run_wasm_execution(
        &mut engine,
        first_context.context_id,
        temp.path(),
        Vec::new(),
        debug_env.clone(),
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0);
    assert!(first_stdout.contains("stdout:wasm-smoke"));
    assert!(first_warmup.executed);
    assert_eq!(first_warmup.reason, "executed");
    assert_eq!(first_warmup.module_path, "./guest.wasm");
    assert!(
        !first_warmup.compile_cache_dir.is_empty(),
        "expected shared compile cache dir in metrics"
    );

    let (second_stdout, second_stderr, second_exit) = run_wasm_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        Vec::new(),
        debug_env,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0);
    assert!(second_stdout.contains("stdout:wasm-smoke"));
    assert!(!second_warmup.executed);
    assert_eq!(second_warmup.reason, "cached");
    assert_eq!(
        second_warmup.compile_cache_dir,
        first_warmup.compile_cache_dir
    );
}

#[test]
fn wasm_warmup_metrics_encode_emoji_module_paths_as_json() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let module_name = "guest-😀.wasm";
    write_fixture(&temp.path().join(module_name), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(format!("./{module_name}")),
    });

    let (stdout, stderr, exit_code) = run_wasm_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::from([(
            String::from("AGENT_OS_WASM_WARMUP_DEBUG"),
            String::from("1"),
        )]),
    );
    let warmup = parse_warmup_metrics(&stderr);

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stdout.contains("stdout:wasm-smoke"));
    assert!(warmup.executed, "stderr: {stderr}");
    assert_eq!(warmup.module_path, format!("./{module_name}"));
    assert!(stderr.contains("\\ud83d\\ude00"), "stderr: {stderr}");
}
