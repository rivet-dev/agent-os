mod support;

use agentos_execution::{CreateWasmContextRequest, StartWasmExecutionRequest, WasmPermissionTier};
use std::{collections::BTreeMap, fs};
use tempfile::tempdir;

fn preview1_memory_bounds_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (type $fd_write_t (func (param i32 i32 i32 i32) (result i32)))
  (type $poll_oneoff_t (func (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (type $fd_write_t)))
  (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll_oneoff (type $poll_oneoff_t)))
  (memory (export "memory") 1)
  (data (i32.const 256) "must-not-write")
  (data (i32.const 280) "bounds-ok\0a")
  (func $_start (export "_start")
    ;; The first iovec is valid but the second is not. fd_write must return
    ;; EFAULT without consuming or emitting the valid prefix.
    (i32.store (i32.const 0) (i32.const 256))
    (i32.store (i32.const 4) (i32.const 14))
    (i32.store (i32.const 8) (i32.const 65530))
    (i32.store (i32.const 12) (i32.const 16))
    (if
      (i32.ne
        (call $fd_write (i32.const 1) (i32.const 0) (i32.const 2) (i32.const 200))
        (i32.const 21)
      )
      (then unreachable)
    )

    ;; Linux-compatible IOV_MAX is enforced before reading the table.
    (if
      (i32.ne
        (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1025) (i32.const 200))
        (i32.const 28)
      )
      (then unreachable)
    )

    ;; A zero-time clock subscription must still validate the complete output
    ;; event table before waiting or attempting a partial event copyout.
    (i64.store (i32.const 64) (i64.const 7))
    (i32.store8 (i32.const 72) (i32.const 0))
    (i64.store (i32.const 88) (i64.const 0))
    (if
      (i32.ne
        (call $poll_oneoff
          (i32.const 64)
          (i32.const 65530)
          (i32.const 1)
          (i32.const 200)
        )
        (i32.const 21)
      )
      (then unreachable)
    )

    (i32.store (i32.const 32) (i32.const 280))
    (i32.store (i32.const 36) (i32.const 10))
    (if
      (i32.ne
        (call $fd_write (i32.const 1) (i32.const 32) (i32.const 1) (i32.const 200))
        (i32.const 0)
      )
      (then unreachable)
    )
  )
)
"#,
    )
    .expect("compile Preview1 memory-bounds wasm fixture")
}

#[test]
fn invalid_preview1_memory_faults_before_host_work_or_copyout() {
    let temp = tempdir().expect("create temp dir");
    fs::write(
        temp.path().join("guest.wasm"),
        preview1_memory_bounds_module(),
    )
    .expect("write wasm fixture");

    let mut engine = support::wasm_engine();
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm-bounds"),
        module_path: Some(String::from("./guest.wasm")),
    });
    let execution = engine
        .start_execution(StartWasmExecutionRequest {
            guest_runtime: Default::default(),
            limits: Default::default(),
            vm_id: String::from("vm-wasm-bounds"),
            context_id: context.context_id,
            managed_kernel_host: false,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: temp.path().to_path_buf(),
            permission_tier: WasmPermissionTier::Full,
        })
        .expect("start wasm execution");

    let result = execution.wait().expect("wait for wasm execution");
    let stdout = String::from_utf8(result.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(result.stderr).expect("stderr utf8");
    assert_eq!(result.exit_code, 0, "stderr={stderr}");
    assert_eq!(stdout, "bounds-ok\n");
}
