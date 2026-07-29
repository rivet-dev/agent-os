use std::{collections::BTreeMap, fs, process::Command, time::Duration};

use agentos_execution::{
    CreateWasmContextRequest, JavascriptSyncRpcRequest, StartWasmExecutionRequest, WasmExecution,
    WasmExecutionEngine, WasmExecutionEvent, WasmPermissionTier,
};
use base64::Engine;
use serde_json::{json, Value};
use tempfile::tempdir;

fn module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_read_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $fd_fdstat_set_flags_t (func (param i32 i32) (result i32)))
  (type $proc_exit_t (func (param i32)))
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (type $fd_read_t)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "fd_fdstat_set_flags" (func $fd_fdstat_set_flags (type $fd_fdstat_set_flags_t)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (type $proc_exit_t)))
  (memory (export "memory") 1)
  (data (i32.const 64) "nonblock-ready\nblocking-ready\n")
  (func $write (param $ptr i32) (param $len i32)
    (i32.store (i32.const 16) (local.get $ptr))
    (i32.store (i32.const 20) (local.get $len))
    (drop (call $fd_write (i32.const 1) (i32.const 16) (i32.const 1) (i32.const 24)))
  )
  (func $_start (export "_start")
    (local $errno i32)
    (i32.store (i32.const 0) (i32.const 32))
    (i32.store (i32.const 4) (i32.const 1))
    (if (i32.ne (call $fd_fdstat_set_flags (i32.const 0) (i32.const 4)) (i32.const 0))
      (then (call $proc_exit (i32.const 41))))
    (local.set $errno (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
    (if (i32.ne (local.get $errno) (i32.const 6))
      (then (call $proc_exit (i32.const 42))))
    (call $write (i32.const 64) (i32.const 15))
    (if (i32.ne (call $fd_fdstat_set_flags (i32.const 0) (i32.const 0)) (i32.const 0))
      (then (call $proc_exit (i32.const 43))))
    (local.set $errno (call $fd_read (i32.const 0) (i32.const 0) (i32.const 1) (i32.const 8)))
    (if (i32.ne (local.get $errno) (i32.const 0))
      (then (call $proc_exit (i32.const 44))))
    (if (i32.ne (i32.load (i32.const 8)) (i32.const 1))
      (then (call $proc_exit (i32.const 45))))
    (call $write (i32.const 79) (i32.const 15))
  )
)
"#,
    )
    .expect("compile WASI stdin fixture")
}

fn request(execution: &mut WasmExecution) -> JavascriptSyncRpcRequest {
    match execution
        .poll_event_blocking(Duration::from_secs(5))
        .expect("poll WASM event")
    {
        Some(WasmExecutionEvent::SyncRpcRequest(request)) => request,
        other => panic!("expected sync RPC request, got {other:?}"),
    }
}

fn request_bytes(request: &JavascriptSyncRpcRequest) -> Vec<u8> {
    let encoded = request.args[1]
        .get("base64")
        .and_then(Value::as_str)
        .expect("sync RPC byte payload");
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("decode sync RPC bytes")
}

#[test]
fn fd0_nonblocking_returns_eagain_without_starving_progress_and_blocking_still_waits() {
    let node = std::env::var("AGENTOS_NODE_BINARY").unwrap_or_else(|_| "node".into());
    assert!(Command::new(node)
        .arg("--version")
        .status()
        .unwrap()
        .success());

    let temp = tempdir().expect("create temp dir");
    fs::write(temp.path().join("guest.wasm"), module()).expect("write WASM fixture");
    let mut engine = WasmExecutionEngine::default();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: "vm-wasm-nonblock".into(),
        module_path: Some("./guest.wasm".into()),
    });
    let mut execution = engine
        .start_execution(StartWasmExecutionRequest {
            guest_runtime: Default::default(),
            limits: Default::default(),
            vm_id: "vm-wasm-nonblock".into(),
            context_id: context.context_id,
            argv: Vec::new(),
            env: BTreeMap::from([("AGENTOS_WASI_STDIO_SYNC_RPC".into(), "1".into())]),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start WASM fixture");

    let read = request(&mut execution);
    assert_eq!(read.method, "__kernel_stdin_read");
    assert_eq!(read.args.get(1), Some(&json!(0)));
    execution
        .respond_sync_rpc_success(read.id, Value::Null)
        .expect("respond to empty nonblocking read");

    let progress = request(&mut execution);
    assert_eq!(progress.method, "__kernel_stdio_write");
    assert_eq!(request_bytes(&progress), b"nonblock-ready\n");
    execution
        .respond_sync_rpc_success(progress.id, json!(15))
        .expect("ack progress marker");

    let read = request(&mut execution);
    assert_eq!(read.method, "__kernel_stdin_read");
    assert_eq!(read.args.get(1), Some(&json!(10_000)));
    assert!(execution
        .poll_event_blocking(Duration::from_millis(25))
        .expect("poll parked blocking read")
        .is_none());
    execution
        .respond_sync_rpc_success(
            read.id,
            json!({
                "dataBase64": base64::engine::general_purpose::STANDARD.encode(b"x"),
            }),
        )
        .expect("deliver blocking read byte");

    let progress = request(&mut execution);
    assert_eq!(progress.method, "__kernel_stdio_write");
    assert_eq!(request_bytes(&progress), b"blocking-ready\n");
    execution
        .respond_sync_rpc_success(progress.id, json!(15))
        .expect("ack blocking marker");
    let result = execution.wait().expect("wait for WASM fixture");
    assert_eq!(
        result.exit_code,
        0,
        "stderr={}",
        String::from_utf8_lossy(&result.stderr)
    );
}
