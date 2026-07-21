mod support;

use std::collections::HashMap;
use std::time::Duration;

use agentos_native_sidecar::wire::{
    ExecuteRequest, GuestRuntimeKind, KillProcessRequest, RequestPayload, ResponsePayload,
    StandaloneWasmBackend, WasmPermissionTier,
};
use support::{
    authenticate_wire, collect_process_output_wire_with_timeout, new_sidecar, open_session_wire,
    temp_dir, wire_request, wire_vm, write_fixture,
};

fn run_wasmtime(
    name: &str,
    module: &[u8],
    metadata: HashMap<String, String>,
) -> (String, String, i32) {
    run_wasmtime_with_tier(name, module, metadata, WasmPermissionTier::Isolated)
}

fn run_wasmtime_with_tier(
    name: &str,
    module: &[u8],
    metadata: HashMap<String, String>,
    permission_tier: WasmPermissionTier,
) -> (String, String, i32) {
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("fixture.wasm");
    write_fixture(&entrypoint, module);
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-safety");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        metadata,
    );
    let process_id = format!("process-{name}");
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.clone(),
                command: None,
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: Some(permission_tier),
                wasm_backend: Some(StandaloneWasmBackend::Wasmtime),
            }),
        ))
        .expect("start Wasmtime safety fixture");
    assert!(matches!(
        started.response.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));
    collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(10),
    )
}

#[test]
fn permission_filter_denies_ungranted_host_families_without_ambient_fallback() {
    let denied = wat::parse_str(
        r#"(module
            (import "host_process" "sleep_ms" (func $sleep (param i32) (result i32)))
            (func (export "_start") (drop (call $sleep (i32.const 1)))))"#,
    )
    .expect("denied import fixture");
    let (_, stderr, exit_code) = run_wasmtime("wasmtime-denied-import", &denied, HashMap::new());
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("host_process.sleep_ms"), "stderr: {stderr}");
}

#[test]
fn active_cpu_budget_pauses_during_async_kernel_waits() {
    let waiting = wat::parse_str(
        r#"(module
            (import "host_process" "sleep_ms" (func $sleep (param i32) (result i32)))
            (func (export "_start")
                (if (i32.ne (call $sleep (i32.const 100)) (i32.const 0))
                    (then unreachable))))"#,
    )
    .expect("async wait fixture");
    let (stdout, stderr, exit_code) = run_wasmtime_with_tier(
        "wasmtime-active-cpu-paused-wait",
        &waiting,
        HashMap::from([(
            String::from("limits.wasm.active_cpu_time_limit_ms"),
            String::from("20"),
        )]),
        WasmPermissionTier::Full,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

fn infinite_loop_module() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (func (export "_start")
                (loop $forever (br $forever))))"#,
    )
    .expect("infinite-loop fixture")
}

#[test]
fn malformed_and_hostile_imports_fail_with_stable_typed_errors() {
    let (_, stderr, exit_code) =
        run_wasmtime("wasmtime-malformed", b"\0asm\x01\0\0", HashMap::new());
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASM_INVALID_MODULE"),
        "stderr: {stderr}"
    );

    let hostile = wat::parse_str(
        r#"(module
            (import "env" "ambient_host_escape" (func $escape))
            (func (export "_start") (call $escape)))"#,
    )
    .expect("hostile import fixture");
    let (_, stderr, exit_code) = run_wasmtime("wasmtime-hostile-import", &hostile, HashMap::new());
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASM_UNSUPPORTED_IMPORT"),
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("unknown import"), "stderr: {stderr}");
}

#[test]
fn memory_table_stack_fuel_cpu_and_wall_limits_fail_closed() {
    let exact_memory =
        wat::parse_str(r#"(module (memory (export "memory") 1) (func (export "_start")))"#)
            .expect("exact-memory fixture");
    let (stdout, stderr, exit_code) = run_wasmtime(
        "wasmtime-memory-at-limit",
        &exact_memory,
        HashMap::from([(
            String::from("resource.max_wasm_memory_bytes"),
            String::from("65536"),
        )]),
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");

    let oversized_memory =
        wat::parse_str(r#"(module (memory (export "memory") 2) (func (export "_start")))"#)
            .expect("memory-limit fixture");
    let (_, stderr, exit_code) = run_wasmtime(
        "wasmtime-memory-limit",
        &oversized_memory,
        HashMap::from([(
            String::from("resource.max_wasm_memory_bytes"),
            String::from("65536"),
        )]),
    );
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASMTIME_MEMORY_LIMIT"),
        "stderr: {stderr}"
    );

    let oversized_table =
        wat::parse_str(r#"(module (table 1000001 funcref) (func (export "_start")))"#)
            .expect("table-limit fixture");
    let (_, stderr, exit_code) =
        run_wasmtime("wasmtime-table-limit", &oversized_table, HashMap::new());
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASMTIME_TABLE_LIMIT"),
        "stderr: {stderr}"
    );

    let recursive = wat::parse_str(
        r#"(module
            (func $recurse (call $recurse))
            (func (export "_start") (call $recurse)))"#,
    )
    .expect("stack-limit fixture");
    let (_, stderr, exit_code) = run_wasmtime(
        "wasmtime-stack-limit",
        &recursive,
        HashMap::from([(
            String::from("resource.max_wasm_stack_bytes"),
            String::from("65536"),
        )]),
    );
    assert_eq!(exit_code, 1);
    assert!(
        stderr.contains("ERR_AGENTOS_WASMTIME_STACK_EXHAUSTED"),
        "stderr: {stderr}"
    );

    for (name, metadata_key, value, expected) in [
        (
            "fuel",
            "limits.wasm.deterministic_fuel",
            "1000",
            "ERR_AGENTOS_WASMTIME_FUEL_EXHAUSTED",
        ),
        (
            "active-cpu",
            "limits.wasm.active_cpu_time_limit_ms",
            "20",
            "ERR_AGENTOS_WASMTIME_ACTIVE_CPU_LIMIT",
        ),
        (
            "wall-clock",
            "limits.wasm.wall_clock_limit_ms",
            "20",
            "ERR_AGENTOS_WASMTIME_WALL_CLOCK_LIMIT",
        ),
    ] {
        let (_, stderr, exit_code) = run_wasmtime(
            &format!("wasmtime-{name}-limit"),
            &infinite_loop_module(),
            HashMap::from([(metadata_key.to_owned(), value.to_owned())]),
        );
        assert_eq!(exit_code, 1, "{name} stderr: {stderr}");
        assert!(stderr.contains(expected), "{name} stderr: {stderr}");
    }
}

#[test]
fn terminal_signal_interrupts_and_reaps_pure_guest_compute() {
    let name = "wasmtime-terminal-signal";
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("loop.wasm");
    write_fixture(&entrypoint, infinite_loop_module());
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-kill");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
    );
    let process_id = String::from("process-wasmtime-loop");
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            4,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.clone(),
                command: None,
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: Some(WasmPermissionTier::Isolated),
                wasm_backend: Some(StandaloneWasmBackend::Wasmtime),
            }),
        ))
        .expect("start pure-compute Wasmtime process");
    assert!(matches!(
        started.response.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));

    let killed = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: process_id.clone(),
                signal: String::from("SIGKILL"),
            }),
        ))
        .expect("kill pure-compute Wasmtime process");
    assert!(matches!(
        killed.response.payload,
        ResponsePayload::ProcessKilledResponse(_)
    ));
    let (_, _, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 137);
}
