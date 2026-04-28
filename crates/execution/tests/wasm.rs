use agent_os_execution::wasm::{
    NativeBinaryFormat, WASM_MAX_FUEL_ENV, WASM_MAX_MEMORY_BYTES_ENV, WASM_PREWARM_TIMEOUT_MS_ENV,
};
use agent_os_execution::{
    CreateWasmContextRequest, StartWasmExecutionRequest, WasmExecutionEngine, WasmExecutionError,
    WasmExecutionEvent, WasmPermissionTier,
};
use base64::Engine;
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tempfile::tempdir;

const WASM_WARMUP_METRICS_PREFIX: &str = "__AGENT_OS_WASM_WARMUP_METRICS__:";

fn node_binary_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn lock_node_binary_env() -> MutexGuard<'static, ()> {
    node_binary_env_lock()
        .lock()
        .expect("lock AGENT_OS_NODE_BINARY test guard")
}

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var(key).ok();
        // SAFETY: These tests mutate process env only within this scoped guard.
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
                // SAFETY: Restores the env key owned by this scoped guard.
                unsafe {
                    std::env::set_var(self.key, value);
                }
            }
            None => {
                // SAFETY: Restores the env key owned by this scoped guard.
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WasmWarmupMetrics {
    executed: bool,
    reason: String,
    module_path: String,
    compile_cache_dir: String,
}

fn assert_node_available() {
    let _guard = lock_node_binary_env();
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

fn decode_sync_rpc_bytes(value: &serde_json::Value) -> Vec<u8> {
    let base64 = value
        .get("__agentOsType")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == "bytes")
        .then(|| value.get("base64").and_then(serde_json::Value::as_str))
        .flatten()
        .expect("sync rpc bytes payload");
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .expect("decode sync rpc bytes")
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
    use std::os::unix::fs::PermissionsExt;
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fake node binary");
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
    permission_tier: WasmPermissionTier,
) -> (String, String, i32) {
    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id,
            argv,
            env,
            cwd: cwd.to_path_buf(),
            permission_tier,
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

fn wasm_stdin_echo_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_read_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (type $fd_read_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (func $_start (export "_start")
    (i32.store (i32.const 0) (i32.const 32))
    (i32.store (i32.const 4) (i32.const 64))
    (drop
      (call $fd_read
        (i32.const 0)
        (i32.const 0)
        (i32.const 1)
        (i32.const 8)
      )
    )
    (i32.store (i32.const 16) (i32.const 32))
    (i32.store (i32.const 20) (i32.load (i32.const 8)))
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
    .expect("compile stdin echo wasm fixture")
}

fn wasm_fdstat_set_flags_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_fdstat_get_t (func (param i32 i32) (result i32)))
  (type $fd_fdstat_set_flags_t (func (param i32 i32) (result i32)))
  (type $proc_exit_t (func (param i32)))
  (import "wasi_snapshot_preview1" "fd_fdstat_get" (func $fd_fdstat_get (type $fd_fdstat_get_t)))
  (import "wasi_snapshot_preview1" "fd_fdstat_set_flags" (func $fd_fdstat_set_flags (type $fd_fdstat_set_flags_t)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (type $proc_exit_t)))
  (memory (export "memory") 1)
  (func $_start (export "_start")
    (if
      (i32.ne
        (call $fd_fdstat_set_flags (i32.const 1) (i32.const 4))
        (i32.const 0)
      )
      (then (call $proc_exit (i32.const 41)))
    )
    (if
      (i32.ne
        (call $fd_fdstat_get (i32.const 1) (i32.const 0))
        (i32.const 0)
      )
      (then (call $proc_exit (i32.const 42)))
    )
    (if
      (i32.ne
        (i32.load16_u offset=2 (i32.const 0))
        (i32.const 4)
      )
      (then (call $proc_exit (i32.const 43)))
    )
  )
)
"#,
    )
    .expect("compile fdstat flags wasm fixture")
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

fn wat_escape_ascii(input: &str) -> String {
    let mut escaped = String::new();
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\0d"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn wasm_stdout_chunks_module(chunks: &[&str]) -> Vec<u8> {
    let mut data_offset = 64u32;
    let mut data_segments = String::new();
    let mut writes = String::new();

    for (index, chunk) in chunks.iter().enumerate() {
        let escaped = wat_escape_ascii(chunk);
        let chunk_len = chunk.len();
        let iovec_offset = (index as u32) * 8;
        data_segments.push_str(&format!(
            "  (data (i32.const {data_offset}) \"{escaped}\")\n"
        ));
        writes.push_str(&format!(
            "    (i32.store (i32.const {iovec_offset}) (i32.const {data_offset}))\n    (i32.store (i32.const {}) (i32.const {chunk_len}))\n    (drop\n      (call $fd_write\n        (i32.const 1)\n        (i32.const {iovec_offset})\n        (i32.const 1)\n        (i32.const 40)\n      )\n    )\n",
            iovec_offset + 4
        ));
        data_offset += chunk_len as u32;
    }

    wat::parse_str(&format!(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
{data_segments}  (func $_start (export "_start")
{writes}  )
)
"#
    ))
    .expect("compile stdout-chunks wasm fixture")
}

fn wasm_signal_state_line_stdout_module() -> Vec<u8> {
    wasm_stdout_chunks_module(&[
        "hello\n__AGENT_OS_WASM_SIGNAL_STATE__:{\"signal\":2,\"registration\":{\"action\":\"user\",\"mask\":[15],\"flags\":4660}}\n",
    ])
}

fn wasm_split_signal_state_line_stdout_module() -> Vec<u8> {
    wasm_stdout_chunks_module(&[
        "__AGENT_OS_WASM_SIGNAL_STATE__:",
        "{\"signal\":2,\"registration\":{\"action\":\"user\",\"mask\":[15],\"flags\":4660}}\n",
    ])
}

fn wasm_write_file_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $path_open_t (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_close_t (func (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (type $path_open_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (type $fd_close_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "output.txt")
  (data (i32.const 80) "tiered-write\n")
  (func $_start (export "_start")
    (if
      (i32.ne
        (call $path_open
          (i32.const 3)
          (i32.const 0)
          (i32.const 64)
          (i32.const 10)
          (i32.const 9)
          (i64.const 64)
          (i64.const 64)
          (i32.const 0)
          (i32.const 8)
        )
        (i32.const 0)
      )
      (then unreachable)
    )
    (i32.store (i32.const 0) (i32.const 80))
    (i32.store (i32.const 4) (i32.const 13))
    (if
      (i32.ne
        (call $fd_write
          (i32.load (i32.const 8))
          (i32.const 0)
          (i32.const 1)
          (i32.const 12)
        )
        (i32.const 0)
      )
      (then unreachable)
    )
    (drop (call $fd_close (i32.load (i32.const 8))))
  )
)
"#,
    )
    .expect("compile write-file wasm fixture")
}

fn wasm_write_nested_file_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $path_open_t (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_close_t (func (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (type $path_open_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (type $fd_close_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "nested/output.txt")
  (data (i32.const 96) "nested-write\n")
  (func $_start (export "_start")
    (if
      (i32.ne
        (call $path_open
          (i32.const 3)
          (i32.const 0)
          (i32.const 64)
          (i32.const 17)
          (i32.const 9)
          (i64.const 64)
          (i64.const 64)
          (i32.const 0)
          (i32.const 8)
        )
        (i32.const 0)
      )
      (then unreachable)
    )
    (i32.store (i32.const 0) (i32.const 96))
    (i32.store (i32.const 4) (i32.const 13))
    (if
      (i32.ne
        (call $fd_write
          (i32.load (i32.const 8))
          (i32.const 0)
          (i32.const 1)
          (i32.const 12)
        )
        (i32.const 0)
      )
      (then unreachable)
    )
    (drop (call $fd_close (i32.load (i32.const 8))))
  )
)
"#,
    )
    .expect("compile nested write-file wasm fixture")
}

fn wasm_expect_write_open_errno_module(expected_errno: u32) -> Vec<u8> {
    wat::parse_str(&format!(
        r#"
(module
  (type $path_open_t (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (type $fd_close_t (func (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (type $path_open_t)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (type $fd_close_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "output.txt")
  (func $_start (export "_start")
    (local $errno i32)
    (local.set $errno
      (call $path_open
        (i32.const 3)
        (i32.const 0)
        (i32.const 64)
        (i32.const 10)
        (i32.const 9)
        (i64.const 64)
        (i64.const 64)
        (i32.const 0)
        (i32.const 8)
      )
    )
    (if
      (i32.ne
        (local.get $errno)
        (i32.const {expected_errno})
      )
      (then unreachable)
    )
    (if
      (i32.eq (local.get $errno) (i32.const 0))
      (then
        (drop (call $fd_close (i32.load (i32.const 8))))
      )
    )
  )
)
"#
    ))
    .expect("compile expected-errno wasm fixture")
}

fn wasm_escape_preopen_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $path_open_t (func (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (type $path_open_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "../../../../etc/passwd")
  (func $_start (export "_start")
    (if
      (i32.ne
        (call $path_open
          (i32.const 3)
          (i32.const 0)
          (i32.const 64)
          (i32.const 22)
          (i32.const 0)
          (i64.const 0)
          (i64.const 0)
          (i32.const 0)
          (i32.const 8)
        )
        (i32.const 44)
      )
      (then unreachable)
    )
  )
)
"#,
    )
    .expect("compile preopen-escape wasm fixture")
}

fn wasm_poll_oneoff_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $poll_oneoff_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll_oneoff (type $poll_oneoff_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 176) "poll-ready\n")
  (func $_start (export "_start")
    (i64.store (i32.const 0) (i64.const 1))
    (i32.store8 (i32.const 8) (i32.const 1))
    (i32.store (i32.const 16) (i32.const 0))

    (i64.store (i32.const 48) (i64.const 2))
    (i32.store8 (i32.const 56) (i32.const 1))
    (i32.store (i32.const 64) (i32.const 1))

    (if
      (i32.ne
        (call $poll_oneoff
          (i32.const 0)
          (i32.const 96)
          (i32.const 2)
          (i32.const 160)
        )
        (i32.const 0)
      )
      (then unreachable)
    )

    (if (i32.ne (i32.load (i32.const 160)) (i32.const 1)) (then unreachable))
    (if (i64.ne (i64.load (i32.const 96)) (i64.const 1)) (then unreachable))
    (if (i32.ne (i32.load8_u (i32.const 106)) (i32.const 1)) (then unreachable))

    (i32.store (i32.const 168) (i32.const 176))
    (i32.store (i32.const 172) (i32.const 11))
    (drop
      (call $fd_write
        (i32.const 1)
        (i32.const 168)
        (i32.const 1)
        (i32.const 164)
      )
    )
  )
)
"#,
    )
    .expect("compile poll_oneoff wasm fixture")
}

fn wasm_infinite_loop_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (memory (export "memory") 1)
  (func $_start (export "_start")
    (loop $spin
      br $spin
    )
  )
)
"#,
    )
    .expect("compile infinite-loop wasm fixture")
}

fn wasm_memory_capped_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (memory (export "memory") 1 3)
  (func $_start (export "_start"))
)
"#,
    )
    .expect("compile memory-capped wasm fixture")
}

fn wasm_memory_grow_until_runtime_limit_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (memory (export "memory") 1)
  (data (i32.const 32) "memory-grow-limited\n")
  (func $_start (export "_start")
    (if
      (i32.ne
        (memory.grow (i32.const 1))
        (i32.const 1)
      )
      (then unreachable)
    )
    (if
      (i32.ne
        (memory.grow (i32.const 1))
        (i32.const -1)
      )
      (then unreachable)
    )
    (i32.store (i32.const 0) (i32.const 32))
    (i32.store (i32.const 4) (i32.const 20))
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
    .expect("compile runtime memory-limit wasm fixture")
}

fn raw_wasm_module(section_id: u8, section_contents: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::from(*b"\0asm");
    bytes.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    bytes.push(section_id);
    bytes.extend(encode_varuint(section_contents.len() as u64));
    bytes.extend_from_slice(section_contents);
    bytes
}

fn encode_varuint(mut value: u64) -> Vec<u8> {
    let mut encoded = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        encoded.push(byte);
        if value == 0 {
            return encoded;
        }
    }
}

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

fn wasm_execution_stays_inside_v8_runtime_without_host_node_launches() {
    let _guard = lock_node_binary_env();
    let temp = tempdir().expect("create temp dir");
    let fake_node_path = temp.path().join("fake-node.sh");
    let log_path = temp.path().join("node-invocations.log");
    write_fake_node_binary(&fake_node_path, &log_path);
    let _node_binary = EnvVarGuard::set("AGENT_OS_NODE_BINARY", &fake_node_path);

    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

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
        BTreeMap::from([(
            String::from(WASM_MAX_MEMORY_BYTES_ENV),
            String::from((2 * 65_536).to_string()),
        )]),
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("stdout:wasm-smoke"), "stdout={stdout}");
    assert!(
        !log_path.exists(),
        "WASM prewarm/execution should stay inside the shared V8 runtime, not launch AGENT_OS_NODE_BINARY",
    );
}

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
            permission_tier: WasmPermissionTier::Full,
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

fn wasm_execution_supports_fd_fdstat_set_flags() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_fdstat_set_flags_module(),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let (_stdout, stderr, exit_code) = run_wasm_execution(
        &mut engine,
        context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(
        !stderr.contains("fd_fdstat_set_flags"),
        "missing WASI fd_fdstat_set_flags import should not leak into stderr: {stderr}"
    );
    assert!(
        !stderr.contains("LinkError"),
        "WASI import gaps should not break module instantiation: {stderr}"
    );
}

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
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert_eq!(stdout, "stdout:wasm-smoke\n");
    assert!(!stdout.contains("evil-smoke"));
}

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
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0);
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert!(stdout.contains("timing:frozen"), "stdout: {stdout}");
}

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
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("vm mismatch should fail");

    assert!(error
        .to_string()
        .contains("guest WebAssembly context belongs to vm vm-wasm, not vm-other"));
}

fn wasm_execution_streams_exit_event() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    let mut saw_stdout = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
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
            Some(WasmExecutionEvent::SyncRpcRequest(_)) => {}
            Some(WasmExecutionEvent::SignalState { .. }) => {}
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert!(saw_stdout, "expected stdout event before exit");
}

fn wasm_execution_can_route_stdio_through_kernel_sync_rpc() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdout_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from("AGENT_OS_WASI_STDIO_SYNC_RPC"),
                String::from("1"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll wasm event")
    {
        Some(WasmExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected kernel stdio sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "__kernel_stdio_write");
    assert_eq!(request.args.first(), Some(&json!(1)));
    assert_eq!(
        String::from_utf8(decode_sync_rpc_bytes(&request.args[1])).expect("stdout utf8"),
        "stdout:wasm-smoke\n"
    );

    execution
        .respond_sync_rpc_success(request.id, json!(18))
        .expect("respond to __kernel_stdio_write");

    let result = execution.wait().expect("wait for wasm execution");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stderr={stderr}");
    assert!(
        result.stdout.is_empty(),
        "stdout should be kernel-routed in this mode"
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn wasm_execution_reads_streaming_stdin_via_kernel_bridge() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_stdin_echo_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from("AGENT_OS_WASI_STDIO_SYNC_RPC"),
                String::from("1"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    execution
        .write_stdin(b"stdin-echo\n")
        .expect("write wasm stdin");
    execution.close_stdin().expect("close wasm stdin");

    let result = execution.wait().expect("wait for wasm execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");

    assert_eq!(result.exit_code, 0, "stderr={stderr}");
    assert_eq!(stdout, "stdin-echo\n");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn wasm_execution_poll_oneoff_uses_kernel_poll_for_multiple_fds() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_poll_oneoff_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    let request = match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll wasm event")
    {
        Some(WasmExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    };

    assert_eq!(request.method, "__kernel_poll");
    assert_eq!(
        request.args,
        vec![
            json!([
                { "fd": 0, "events": 1 },
                { "fd": 1, "events": 1 }
            ]),
            json!(10),
        ]
    );

    execution
        .respond_sync_rpc_success(
            request.id,
            json!({
                "readyCount": 1,
                "fds": [
                    { "fd": 0, "events": 1, "revents": 1 },
                    { "fd": 1, "events": 1, "revents": 0 }
                ]
            }),
        )
        .expect("respond to __kernel_poll");

    let result = execution.wait().expect("wait for wasm execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
    assert_eq!(stdout, "poll-ready\n");
}

fn wasm_execution_emits_signal_state_from_control_channel() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_signal_state_module());

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    let mut saw_stdout = false;
    let mut saw_signal = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
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
            Some(WasmExecutionEvent::SyncRpcRequest(_)) => {}
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert!(saw_stdout, "expected stdout event before exit");
    assert!(saw_signal, "expected signal-state event before exit");
}

fn wasm_execution_preserves_stdout_when_signal_state_marker_shares_stdout_chunk() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_signal_state_line_stdout_module(),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::ReadWrite,
        })
        .expect("start wasm execution");

    let mut stdout = Vec::new();
    let mut saw_signal = false;
    let mut saw_exit = false;

    while !saw_exit {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
            .expect("poll wasm event")
        {
            Some(WasmExecutionEvent::Stdout(chunk)) => stdout.push(chunk),
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
            Some(WasmExecutionEvent::SyncRpcRequest(_)) => {}
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert_eq!(stdout, vec![b"hello\n".to_vec()]);
    assert!(saw_signal, "expected signal-state event before exit");
}

fn wasm_execution_reassembles_split_signal_state_marker_across_stdout_chunks() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_split_signal_state_line_stdout_module(),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::ReadWrite,
        })
        .expect("start wasm execution");

    let mut saw_signal = false;
    let mut saw_exit = false;
    let mut stdout = Vec::new();

    while !saw_exit {
        match execution
            .poll_event_blocking(Duration::from_secs(5))
            .expect("poll wasm event")
        {
            Some(WasmExecutionEvent::Stdout(chunk)) => stdout.push(chunk),
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
            Some(WasmExecutionEvent::SyncRpcRequest(_)) => {}
            None => panic!("timed out waiting for wasm execution event"),
        }
    }

    assert!(stdout.is_empty(), "split marker should not leak to stdout");
    assert!(
        saw_signal,
        "expected reassembled signal-state event before exit"
    );
}

fn wasm_read_only_tier_blocks_workspace_writes_but_read_write_allows_them() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_write_file_module());

    let mut engine = WasmExecutionEngine::default();
    let read_only_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });
    let read_write_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let (read_only_stdout, read_only_stderr, read_only_exit) = run_wasm_execution(
        &mut engine,
        read_only_context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
        WasmPermissionTier::ReadOnly,
    );

    assert_ne!(
        read_only_exit, 0,
        "read-only tier unexpectedly wrote to workspace: stdout={read_only_stdout} stderr={read_only_stderr}"
    );
    assert!(
        !temp.path().join("output.txt").exists(),
        "read-only tier should not create workspace files"
    );

    let (read_write_stdout, read_write_stderr, read_write_exit) = run_wasm_execution(
        &mut engine,
        read_write_context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
        WasmPermissionTier::ReadWrite,
    );

    assert_eq!(
        read_write_exit, 0,
        "read-write tier should allow workspace writes: stdout={read_write_stdout} stderr={read_write_stderr}"
    );
    assert_eq!(
        fs::read_to_string(temp.path().join("output.txt")).expect("read output"),
        "tiered-write\n"
    );
}

fn wasm_read_only_tier_returns_eacces_for_write_open() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_expect_write_open_errno_module(2),
    );

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
        WasmPermissionTier::ReadOnly,
    );

    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(stderr.is_empty(), "stderr={stderr}");
    assert!(
        !temp.path().join("output.txt").exists(),
        "read-only tier should reject write-open before creating the target"
    );
}

fn wasm_execution_rejects_path_open_escape_outside_preopen() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_escape_preopen_module(),
    );

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
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(stderr.is_empty(), "stderr={stderr}");
}

fn wasm_execution_allows_path_open_for_nested_paths_inside_preopen() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    fs::create_dir_all(temp.path().join("nested")).expect("create nested dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_write_nested_file_module(),
    );

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
        WasmPermissionTier::ReadWrite,
    );

    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(stderr.is_empty(), "stderr={stderr}");
    assert_eq!(
        fs::read_to_string(temp.path().join("nested/output.txt")).expect("read nested output"),
        "nested-write\n"
    );
}

fn wasm_full_tier_exposes_host_process_imports_but_read_write_does_not() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(&temp.path().join("guest.wasm"), &wasm_signal_state_module());

    let mut engine = WasmExecutionEngine::default();
    let full_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });
    let read_write_context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let (full_stdout, full_stderr, full_exit) = run_wasm_execution(
        &mut engine,
        full_context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
        WasmPermissionTier::Full,
    );

    assert_eq!(full_exit, 0, "stderr: {full_stderr}");
    assert!(full_stdout.contains("signal:ready"));

    let (_stdout, stderr, exit_code) = run_wasm_execution(
        &mut engine,
        read_write_context.context_id,
        temp.path(),
        Vec::new(),
        BTreeMap::new(),
        WasmPermissionTier::ReadWrite,
    );

    assert_ne!(
        exit_code, 0,
        "read-write tier should deny host_process imports"
    );
    assert!(
        stderr.contains("host_process") || stderr.contains("proc_sigaction"),
        "unexpected stderr for denied host_process import: {stderr}"
    );
}

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
        WasmPermissionTier::Full,
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
        WasmPermissionTier::Full,
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

fn wasm_execution_rewarms_when_symlink_target_changes_with_same_size_module() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    let stable_link = temp.path().join("guest.wasm");
    write_fixture(&temp.path().join("good.wasm"), &wasm_stdout_module());
    write_fixture(&temp.path().join("evil.wasm"), &wasm_override_module());
    symlink("./good.wasm", &stable_link).expect("create initial wasm symlink");

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
        WasmPermissionTier::Full,
    );
    let first_warmup = parse_warmup_metrics(&first_stderr);

    assert_eq!(first_exit, 0, "stderr: {first_stderr}");
    assert!(first_stdout.contains("stdout:wasm-smoke"));
    assert!(first_warmup.executed, "stderr: {first_stderr}");

    fs::remove_file(&stable_link).expect("remove wasm symlink");
    symlink("./evil.wasm", &stable_link).expect("retarget wasm symlink");

    let (second_stdout, second_stderr, second_exit) = run_wasm_execution(
        &mut engine,
        second_context.context_id,
        temp.path(),
        Vec::new(),
        debug_env,
        WasmPermissionTier::Full,
    );
    let second_warmup = parse_warmup_metrics(&second_stderr);

    assert_eq!(second_exit, 0, "stderr: {second_stderr}");
    assert!(second_stdout.contains("stdout:evil-smoke"));
    assert!(second_warmup.executed, "stderr: {second_stderr}");
    assert_eq!(second_warmup.reason, "executed");
}

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
        WasmPermissionTier::Full,
    );
    let warmup = parse_warmup_metrics(&stderr);

    assert_eq!(exit_code, 0, "stderr: {stderr}");
    assert!(stdout.contains("stdout:wasm-smoke"));
    assert!(warmup.executed, "stderr: {stderr}");
    assert_eq!(warmup.module_path, format!("./{module_name}"));
    assert!(stderr.contains("\\ud83d\\ude00"), "stderr: {stderr}");
}

fn wasm_execution_times_out_when_fuel_budget_is_exhausted() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_infinite_loop_module(),
    );

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
        BTreeMap::from([(String::from(WASM_MAX_FUEL_ENV), String::from("25"))]),
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 124, "stdout={stdout} stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("fuel budget exhausted"),
        "stderr should mention the exhausted fuel budget: {stderr}"
    );
}

fn wasm_execution_allows_prewarm_timeout_to_differ_from_execution_timeout() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_infinite_loop_module(),
    );

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
            (String::from(WASM_MAX_FUEL_ENV), String::from("25")),
            (
                String::from(WASM_PREWARM_TIMEOUT_MS_ENV),
                String::from("1000"),
            ),
        ]),
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 124, "stdout={stdout} stderr={stderr}");
    assert!(stdout.is_empty(), "stdout={stdout}");
    assert!(
        stderr.contains("fuel budget exhausted"),
        "stderr should mention the exhausted fuel budget: {stderr}"
    );
}

fn wasm_execution_rejects_modules_whose_memory_cap_exceeds_limit() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_memory_capped_module(),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                (2 * 65_536_u64).to_string(),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("memory limit should reject oversized module maximum");

    assert!(
        error.to_string().contains("memory maximum"),
        "unexpected error: {error}"
    );
}

fn wasm_execution_enforces_runtime_memory_growth_limit_for_modules_without_declared_maximum() {
    assert_node_available();

    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &wasm_memory_grow_until_runtime_limit_module(),
    );

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
        BTreeMap::from([(
            String::from(WASM_MAX_MEMORY_BYTES_ENV),
            (2 * 65_536_u64).to_string(),
        )]),
        WasmPermissionTier::Full,
    );

    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stderr.is_empty(), "stderr={stderr}");
    assert!(
        stdout.contains("memory-grow-limited"),
        "stdout should confirm runtime memory.grow enforcement: {stdout}"
    );
}

fn wasm_execution_rejects_modules_that_exceed_parser_file_size_cap() {
    let temp = tempdir().expect("create temp dir");
    let module_path = temp.path().join("guest.wasm");
    let file = fs::File::create(&module_path).expect("create oversize wasm file");
    file.set_len(256_u64 * 1024 * 1024 + 1)
        .expect("sparsely size oversize wasm file");

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                String::from("65536"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("oversized module should be rejected before read");

    assert!(
        error
            .to_string()
            .contains("module file size of 268435457 bytes exceeds the configured parser cap"),
        "unexpected error: {error}"
    );
}

fn wasm_execution_rejects_modules_with_too_many_import_entries() {
    let temp = tempdir().expect("create temp dir");
    let mut import_section = encode_varuint(16_385);
    import_section.extend_from_slice(&[0x00, 0x00]);
    write_fixture(
        &temp.path().join("guest.wasm"),
        &raw_wasm_module(2, &import_section),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                String::from("65536"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("import cap should reject oversized import section");

    assert!(
        error
            .to_string()
            .contains("import section contains 16385 entries"),
        "unexpected error: {error}"
    );
}

fn wasm_execution_rejects_modules_with_too_many_memory_entries() {
    let temp = tempdir().expect("create temp dir");
    write_fixture(
        &temp.path().join("guest.wasm"),
        &raw_wasm_module(5, &encode_varuint(1_025)),
    );

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                String::from("65536"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("memory cap should reject oversized memory section");

    assert!(
        error
            .to_string()
            .contains("memory section contains 1025 entries"),
        "unexpected error: {error}"
    );
}

fn wasm_execution_rejects_varuints_that_exceed_parser_iteration_cap() {
    let temp = tempdir().expect("create temp dir");
    let mut bytes = Vec::from(*b"\0asm");
    bytes.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    bytes.push(5);
    bytes.extend_from_slice(&[0x80; 11]);
    bytes.push(0x00);
    write_fixture(&temp.path().join("guest.wasm"), &bytes);

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./guest.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([(
                String::from(WASM_MAX_MEMORY_BYTES_ENV),
                String::from("65536"),
            )]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("varuint cap should reject oversized encodings");

    assert!(
        error
            .to_string()
            .contains("varuint exceeds the parser cap of 10 bytes"),
        "unexpected error: {error}"
    );
}

// Regression for US-090: a resolved WebAssembly module that turns out to be a
// `node_modules/.bin/<cmd>` shell-shim script must be rejected by the WASM
// engine with a typed `NonWasmBinary` error BEFORE V8 ever sees the bytes. The
// pre-fix behavior was to base64-encode the `#!/bin/sh` bytes into the prewarm
// env and hand them to `WebAssembly.compile()`, which failed with the opaque
// `CompileError: WebAssembly.Module(): expected magic word 00 61 73 6d, found
// 23 21 2f 62 @+0` cascade that blocked US-088. See
// `.agent/specs/us-090-wasm-warmup-shebang-fix.md` for the full story.
fn wasm_execution_rejects_shell_shim_before_handing_bytes_to_v8() {
    let temp = tempdir().expect("create temp dir");
    let node_modules_bin = temp.path().join("node_modules").join(".bin");
    fs::create_dir_all(&node_modules_bin).expect("create node_modules/.bin");
    let shim_path = node_modules_bin.join("fake-shim");
    let shim_script = "#!/bin/sh\n\
basedir=$(dirname \"$(echo \"$0\" | sed -e 's,\\\\,/,g')\")\n\
\n\
case `uname` in\n\
    *CYGWIN*|*MINGW*|*MSYS*) basedir=`cygpath -w \"$basedir\"`;;\n\
esac\n\
\n\
if [ -x \"$basedir/node\" ]; then\n\
  exec \"$basedir/node\"  \"$basedir/../fake/dist/cli.js\" \"$@\"\n\
else\n\
  exec node  \"$basedir/../fake/dist/cli.js\" \"$@\"\n\
fi\n";
    fs::write(&shim_path, shim_script).expect("write shell-shim fixture");
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(&shim_path)
        .expect("shim metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&shim_path, permissions).expect("chmod shell-shim");

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(shim_path.to_string_lossy().into_owned()),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: vec![shim_path.to_string_lossy().into_owned()],
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("shell shim should be rejected before prewarm/V8");

    match &error {
        agent_os_execution::WasmExecutionError::NonWasmBinary {
            path,
            header,
            shell_shim,
        } => {
            assert!(
                *shell_shim,
                "expected shell_shim=true for shebang header, got header {header:?}"
            );
            assert!(
                header.starts_with(b"#!"),
                "expected header to begin with '#!', got {header:?}"
            );
            assert!(
                path.ends_with("node_modules/.bin/fake-shim"),
                "expected rejected path to name the shim, got {path:?}"
            );
        }
        other => panic!("expected NonWasmBinary typed error, got {other:?}"),
    }

    let rendered = error.to_string();
    assert!(
        !rendered.contains("CompileError"),
        "rendered error must not mention CompileError (got: {rendered})"
    );
    assert!(
        !rendered.contains("WebAssembly.Module()"),
        "rendered error must not mention WebAssembly.Module() (got: {rendered})"
    );
    assert!(
        rendered.contains("node_modules/.bin/fake-shim"),
        "rendered error must name the resolved shim path (got: {rendered})"
    );
    assert!(
        rendered.contains("shell-shim"),
        "rendered error must describe the shell-shim classification (got: {rendered})"
    );
}

fn wasm_execution_rejects_random_non_wasm_bytes_with_typed_error() {
    let temp = tempdir().expect("create temp dir");
    let module_path = temp.path().join("not-really.wasm");
    fs::write(&module_path, b"hello world, definitely not wasm\n").expect("write non-wasm fixture");

    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm"),
        module_path: Some(String::from("./not-really.wasm")),
    });

    let error = engine
        .start_execution(StartWasmExecutionRequest {
            vm_id: String::from("vm-wasm"),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect_err("non-wasm file should be rejected before prewarm/V8");

    match &error {
        agent_os_execution::WasmExecutionError::NonWasmBinary {
            header, shell_shim, ..
        } => {
            assert!(
                !*shell_shim,
                "expected shell_shim=false for non-#! header, got header {header:?}"
            );
            assert_eq!(
                header.as_slice(),
                b"hell",
                "expected first 4 bytes of the fixture, got {header:?}"
            );
        }
        other => panic!("expected NonWasmBinary typed error, got {other:?}"),
    }

    let rendered = error.to_string();
    assert!(
        !rendered.contains("CompileError"),
        "rendered error must not mention CompileError (got: {rendered})"
    );
}

fn wasm_execution_rejects_native_binary_headers_with_explicit_error() {
    for (file_name, header, expected_format) in [
        (
            "fake-elf.wasm",
            b"\x7fELF\x02\x01\x01\x00".as_slice(),
            NativeBinaryFormat::Elf,
        ),
        (
            "fake-macho.wasm",
            b"\xfe\xed\xfa\xcf\x00\x00\x00\x00".as_slice(),
            NativeBinaryFormat::MachO,
        ),
        (
            "fake-pe.wasm",
            b"MZ\x90\x00\x03\x00\x00\x00".as_slice(),
            NativeBinaryFormat::PeCoff,
        ),
    ] {
        let temp = tempdir().expect("create temp dir");
        let module_path = temp.path().join(file_name);
        fs::write(&module_path, header).expect("write native-binary fixture");

        let mut engine = WasmExecutionEngine::default();
        let context = engine.create_context(CreateWasmContextRequest {
            vm_id: String::from("vm-wasm"),
            module_path: Some(format!("./{file_name}")),
        });

        let error = engine
            .start_execution(StartWasmExecutionRequest {
                vm_id: String::from("vm-wasm"),
                context_id: context.context_id,
                argv: Vec::new(),
                env: BTreeMap::new(),
                cwd: temp.path().to_path_buf(),
                permission_tier: WasmPermissionTier::Full,
            })
            .expect_err("native binary should be rejected before prewarm/V8");

        match &error {
            WasmExecutionError::NativeBinaryNotSupported {
                header: observed_header,
                format,
                ..
            } => {
                assert_eq!(*format, expected_format);
                assert_eq!(
                    observed_header.as_slice(),
                    &header[..4],
                    "expected rejected header bytes for {file_name}"
                );
            }
            other => panic!("expected NativeBinaryNotSupported typed error, got {other:?}"),
        }

        let rendered = error.to_string();
        assert!(
            rendered.contains("ERR_NATIVE_BINARY_NOT_SUPPORTED"),
            "rendered error must expose the explicit native-binary code (got: {rendered})"
        );
        assert!(
            rendered.contains(expected_format_display(expected_format)),
            "rendered error must name the detected binary format (got: {rendered})"
        );
        assert!(
            !rendered.contains("CompileError"),
            "rendered error must not mention CompileError (got: {rendered})"
        );
    }
}

fn expected_format_display(format: NativeBinaryFormat) -> &'static str {
    match format {
        NativeBinaryFormat::Elf => "ELF",
        NativeBinaryFormat::MachO => "Mach-O",
        NativeBinaryFormat::PeCoff => "PE/COFF",
    }
}

// Separate libtest cases in this binary still trip a V8 teardown/init crash, so
// keep the WASM runtime coverage in one top-level suite until that boundary is fixed.
#[test]
fn wasm_suite() {
    wasm_contexts_preserve_vm_and_module_configuration();
    wasm_execution_stays_inside_v8_runtime_without_host_node_launches();
    wasm_execution_runs_guest_module_through_v8();
    wasm_execution_supports_fd_fdstat_set_flags();
    wasm_execution_ignores_guest_overrides_for_internal_node_env();
    wasm_execution_freezes_wasi_clock_time();
    wasm_execution_rejects_vm_mismatch();
    wasm_execution_streams_exit_event();
    wasm_execution_can_route_stdio_through_kernel_sync_rpc();
    wasm_execution_reads_streaming_stdin_via_kernel_bridge();
    wasm_execution_poll_oneoff_uses_kernel_poll_for_multiple_fds();
    wasm_execution_emits_signal_state_from_control_channel();
    wasm_execution_preserves_stdout_when_signal_state_marker_shares_stdout_chunk();
    wasm_execution_reassembles_split_signal_state_marker_across_stdout_chunks();
    wasm_read_only_tier_blocks_workspace_writes_but_read_write_allows_them();
    wasm_read_only_tier_returns_eacces_for_write_open();
    wasm_execution_rejects_path_open_escape_outside_preopen();
    wasm_execution_allows_path_open_for_nested_paths_inside_preopen();
    wasm_full_tier_exposes_host_process_imports_but_read_write_does_not();
    wasm_execution_reuses_shared_warmup_path_across_contexts();
    wasm_execution_rewarms_when_symlink_target_changes_with_same_size_module();
    wasm_warmup_metrics_encode_emoji_module_paths_as_json();
    wasm_execution_times_out_when_fuel_budget_is_exhausted();
    wasm_execution_allows_prewarm_timeout_to_differ_from_execution_timeout();
    wasm_execution_rejects_modules_whose_memory_cap_exceeds_limit();
    wasm_execution_enforces_runtime_memory_growth_limit_for_modules_without_declared_maximum();
    wasm_execution_rejects_modules_that_exceed_parser_file_size_cap();
    wasm_execution_rejects_modules_with_too_many_import_entries();
    wasm_execution_rejects_modules_with_too_many_memory_entries();
    wasm_execution_rejects_varuints_that_exceed_parser_iteration_cap();
    wasm_execution_rejects_shell_shim_before_handing_bytes_to_v8();
    wasm_execution_rejects_random_non_wasm_bytes_with_typed_error();
    wasm_execution_rejects_native_binary_headers_with_explicit_error();
}
