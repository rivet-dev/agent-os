mod support;

use std::collections::HashMap;
use std::time::Duration;

use agentos_native_sidecar::wire::{
    DisposeReason, DisposeVmRequest, EventPayload, ExecuteRequest, GuestRuntimeKind,
    KillProcessRequest, RequestPayload, ResponsePayload, StandaloneWasmBackend, WasmPermissionTier,
};
use support::{
    authenticate_wire, collect_process_output_wire_with_timeout, new_sidecar, open_session_wire,
    temp_dir, wire_request, wire_session, wire_vm, write_fixture,
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
    run_wasm_backend_with_tier(
        name,
        module,
        metadata,
        permission_tier,
        StandaloneWasmBackend::Wasmtime,
    )
}

fn run_wasm_backend_with_tier(
    name: &str,
    module: &[u8],
    metadata: HashMap<String, String>,
    permission_tier: WasmPermissionTier,
    backend: StandaloneWasmBackend,
) -> (String, String, i32) {
    if backend == StandaloneWasmBackend::WasmtimeThreads {
        std::env::set_var(
            "AGENTOS_WASMTIME_WORKER_PATH",
            env!("CARGO_BIN_EXE_agentos-native-sidecar"),
        );
    }
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
                wasm_backend: Some(backend),
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

fn start_threaded_process(
    sidecar: &mut agentos_native_sidecar::NativeSidecar<support::RecordingBridge>,
    request_id: i64,
    connection_id: &str,
    session_id: &str,
    vm_id: &str,
    process_id: &str,
    entrypoint: &std::path::Path,
) {
    let started = sidecar
        .dispatch_wire_blocking(wire_request(
            request_id,
            wire_vm(connection_id, session_id, vm_id),
            RequestPayload::ExecuteRequest(ExecuteRequest {
                process_id: process_id.to_owned(),
                command: None,
                runtime: Some(GuestRuntimeKind::WebAssembly),
                entrypoint: Some(entrypoint.to_string_lossy().into_owned()),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                wasm_permission_tier: Some(WasmPermissionTier::Full),
                wasm_backend: Some(StandaloneWasmBackend::WasmtimeThreads),
            }),
        ))
        .expect("start threaded Wasmtime process");
    assert!(matches!(
        started.response.payload,
        ResponsePayload::ProcessStartedResponse(_)
    ));
}

fn threaded_wait_forever_module() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1)))))"#,
    )
    .expect("threaded wait-forever fixture")
}

fn threaded_noop_module() -> Vec<u8> {
    wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32))
            (func (export "_start")))"#,
    )
    .expect("threaded no-op fixture")
}

#[test]
fn threaded_backend_accepts_ordinary_single_thread_wasm_commands() {
    let module = wat::parse_str(
        r#"(module
            (memory (export "memory") 1 2)
            (func (export "_start")))"#,
    )
    .expect("ordinary single-thread fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-ordinary-command",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn explicit_threaded_backend_shares_atomic_memory_across_stores() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
                (drop (memory.atomic.notify (i32.const 4) (i32.const 1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 99)) (i32.const 1))
                    (then unreachable))
                (loop $wait
                    (if (i32.lt_u (i32.atomic.load (i32.const 0)) (i32.const 1))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 4) (i32.const 0) (i64.const -1)))
                            (br $wait))))))"#,
    )
    .expect("threaded sidecar fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-atomic",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn process_signal_selects_the_unblocked_pthread_and_settles_its_handler() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (import "host_process" "proc_sigaction"
                (func $sigaction (param i32 i32 i32 i32 i32) (result i32)))
            (import "host_process" "proc_signal_mask_v2"
                (func $sigmask (param i32 i32 i32 i32 i32) (result i32)))
            (import "host_process" "proc_getpid"
                (func $getpid (param i32) (result i32)))
            (import "host_process" "proc_kill"
                (func $kill (param i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "sched_yield"
                (func $yield (result i32)))
            (export "memory" (memory 0))
            (func (export "__wasi_signal_trampoline") (param i32)
                (i32.atomic.store (i32.const 4) (i32.const 1))
                (drop (memory.atomic.notify (i32.const 4) (i32.const 1))))
            (func (export "wasi_thread_start") (param i32 i32)
                (i32.atomic.store (i32.const 0) (i32.const 1))
                (drop (memory.atomic.notify (i32.const 0) (i32.const 1)))
                (loop $dispatch
                    (drop (call $yield))
                    (br_if $dispatch
                        (i32.eqz (i32.atomic.load (i32.const 4))))))
            (func (export "_start")
                (local $pid i32)
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (loop $started
                    (if (i32.eqz (i32.atomic.load (i32.const 0)))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 0) (i32.const 0) (i64.const 100000000)))
                            (br $started))))
                (if (call $sigaction
                        (i32.const 10) (i32.const 2) (i32.const 0)
                        (i32.const 0) (i32.const 0))
                    (then unreachable))
                (if (call $sigmask
                        (i32.const 0) (i32.const 512) (i32.const 0)
                        (i32.const 104) (i32.const 108))
                    (then unreachable))
                (if (call $getpid (i32.const 100)) (then unreachable))
                (local.set $pid (i32.load (i32.const 100)))
                (if (call $kill (local.get $pid) (i32.const 10))
                    (then unreachable))
                (loop $handled
                    (if (i32.eqz (i32.atomic.load (i32.const 4)))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 4) (i32.const 0) (i64.const 100000000)))
                            (br $handled))))))"#,
    )
    .expect("pthread signal-selection fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-signal-selection",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn shared_memory_growth_and_cross_store_visibility_are_group_owned() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (if (i32.ne (memory.grow (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (i32.atomic.store (i32.const 4) (i32.const 1))
                (drop (memory.atomic.notify (i32.const 4) (i32.const 1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (loop $wait
                    (if (i32.eqz (i32.atomic.load (i32.const 4)))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 4) (i32.const 0) (i64.const 100000000)))
                            (br $wait))))
                (if (i32.ne (memory.size) (i32.const 2)) (then unreachable))))"#,
    )
    .expect("shared-memory growth fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-memory-growth",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn thread_admission_fails_transactionally_at_the_configured_group_limit() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (loop $wait
                    (if (i32.eqz (i32.atomic.load (i32.const 0)))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 0) (i32.const 0) (i64.const 100000000)))
                            (br $wait)))))
            (func (export "_start")
                (local $second i32)
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (local.set $second (call $spawn (i32.const 2)))
                (i32.atomic.store (i32.const 0) (i32.const 1))
                (drop (memory.atomic.notify (i32.const 0) (i32.const 8)))
                (if (i32.ge_s (local.get $second) (i32.const 0))
                    (then unreachable))))"#,
    )
    .expect("thread admission fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-admission",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn multiple_thread_groups_run_concurrently_inside_one_vm() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    let name = "wasmtime-threaded-multiple-groups-one-vm";
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let parked_entrypoint = cwd.join("parked.wasm");
    let noop_entrypoint = cwd.join("noop.wasm");
    write_fixture(&parked_entrypoint, threaded_wait_forever_module());
    write_fixture(&noop_entrypoint, threaded_noop_module());
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-multi-group");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([
            (String::from("limits.wasm.max_threads"), String::from("2")),
            (
                String::from("limits.wasm.max_concurrent_threads"),
                String::from("4"),
            ),
        ]),
    );
    let process_ids = ["process-thread-group-a", "process-thread-group-b"];
    for (index, process_id) in process_ids.iter().enumerate() {
        start_threaded_process(
            &mut sidecar,
            4 + index as i64,
            &connection_id,
            &session_id,
            &vm_id,
            process_id,
            &parked_entrypoint,
        );
    }

    // Both groups remain live only if their complete two-thread reservations
    // fit concurrently in the distinct four-thread VM aggregate.
    std::thread::sleep(Duration::from_millis(250));
    for (index, process_id) in process_ids.iter().enumerate() {
        sidecar
            .dispatch_wire_blocking(wire_request(
                10 + index as i64,
                wire_vm(&connection_id, &session_id, &vm_id),
                RequestPayload::KillProcessRequest(KillProcessRequest {
                    process_id: (*process_id).to_owned(),
                    signal: String::from("SIGKILL"),
                }),
            ))
            .expect("kill concurrently admitted threaded group");
        let (_, stderr, exit_code) = collect_process_output_wire_with_timeout(
            &mut sidecar,
            &connection_id,
            &session_id,
            &vm_id,
            process_id,
            Duration::from_secs(5),
        );
        assert_eq!(exit_code, 137, "{process_id}: stderr={stderr}");
    }

    start_threaded_process(
        &mut sidecar,
        20,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-group-after-release",
        &noop_entrypoint,
    );
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-group-after-release",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[cfg(unix)]
#[test]
fn malformed_and_crashed_thread_workers_fail_closed_and_release_the_vm() {
    use std::os::unix::fs::PermissionsExt;

    let name = "wasmtime-threaded-worker-fault-isolation";
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("noop.wasm");
    let fake_worker = cwd.join("fake-worker.sh");
    write_fixture(&entrypoint, threaded_noop_module());
    write_fixture(
        &fake_worker,
        b"#!/bin/sh\nprintf '\\377\\377\\377\\177'\nsleep 10\n",
    );
    let mut permissions = std::fs::metadata(&fake_worker)
        .expect("fake worker metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fake_worker, permissions).expect("make fake worker executable");
    std::env::set_var("AGENTOS_WASMTIME_WORKER_PATH", &fake_worker);

    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-worker-fault");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([
            (String::from("limits.wasm.max_threads"), String::from("2")),
            (
                String::from("limits.wasm.max_concurrent_threads"),
                String::from("2"),
            ),
        ]),
    );

    start_threaded_process(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "process-malformed-worker",
        &entrypoint,
    );
    let (_, malformed_stderr, malformed_exit) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-malformed-worker",
        Duration::from_secs(5),
    );
    assert_ne!(malformed_exit, 0, "malformed worker must fail closed");
    assert!(
        malformed_stderr.contains("ERR_AGENTOS_WASMTIME_WORKER_IPC_LIMIT"),
        "missing typed malformed-frame error: {malformed_stderr}"
    );

    write_fixture(&fake_worker, b"#!/bin/sh\nexit 86\n");
    start_threaded_process(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "process-crashed-worker",
        &entrypoint,
    );
    let (_, crashed_stderr, crashed_exit) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-crashed-worker",
        Duration::from_secs(5),
    );
    assert_ne!(crashed_exit, 0, "crashed worker must fail closed");
    assert!(
        crashed_stderr.contains("ERR_AGENTOS_WASMTIME_WORKER_IPC_")
            || crashed_stderr.contains("ERR_AGENTOS_WASMTIME_WORKER_WAIT"),
        "missing typed crashed-worker error: {crashed_stderr}"
    );

    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    start_threaded_process(
        &mut sidecar,
        6,
        &connection_id,
        &session_id,
        &vm_id,
        "process-after-worker-faults",
        &entrypoint,
    );
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-after-worker-faults",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn atomic_race_stress_preserves_cross_thread_updates() {
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (local $remaining i32)
                (local.set $remaining (i32.const 1000))
                (loop $add
                    (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
                    (local.set $remaining
                        (i32.sub (local.get $remaining) (i32.const 1)))
                    (br_if $add (local.get $remaining)))
                (drop (i32.atomic.rmw.add (i32.const 4) (i32.const 1)))
                (drop (memory.atomic.notify (i32.const 4) (i32.const 1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1)) (then unreachable))
                (if (i32.lt_s (call $spawn (i32.const 2)) (i32.const 1)) (then unreachable))
                (if (i32.lt_s (call $spawn (i32.const 3)) (i32.const 1)) (then unreachable))
                (if (i32.lt_s (call $spawn (i32.const 4)) (i32.const 1)) (then unreachable))
                (loop $wait
                    (if (i32.lt_u (i32.atomic.load (i32.const 4)) (i32.const 4))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 4)
                                (i32.atomic.load (i32.const 4))
                                (i64.const 100000000)))
                            (br $wait))))
                (if (i32.ne (i32.atomic.load (i32.const 0)) (i32.const 4000))
                    (then unreachable))))"#,
    )
    .expect("atomic race fixture");
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-threaded-atomic-race",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("5"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
#[ignore = "requires `make -C toolchain/c pthread-conformance-wasm` generated artifact"]
fn owned_pthread_libc_mutex_cond_tls_join_detach_and_cancel_conform() {
    let artifact = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../toolchain/c/build/pthread_conformance.wasm");
    let module = std::fs::read(&artifact)
        .unwrap_or_else(|error| panic!("generated fixture {}: {error}", artifact.display()));
    let (stdout, stderr, exit_code) = run_wasm_backend_with_tier(
        "wasmtime-pthread-libc",
        &module,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("8"))]),
        WasmPermissionTier::Full,
        StandaloneWasmBackend::WasmtimeThreads,
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("pthread-ok"),
        "stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn secondary_thread_trap_reaps_group_releases_admission_and_preserves_sidecar() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    let name = "wasmtime-threaded-trap-isolation";
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32) unreachable)
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1)))))"#,
    )
    .expect("secondary trap fixture");
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let trap_entrypoint = cwd.join("trap.wasm");
    let noop_entrypoint = cwd.join("noop.wasm");
    write_fixture(&trap_entrypoint, module);
    write_fixture(&noop_entrypoint, threaded_noop_module());
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-trap");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
    );

    let started = std::time::Instant::now();
    start_threaded_process(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-trap",
        &trap_entrypoint,
    );
    let (_, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-trap",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 1, "stderr={stderr}");
    assert!(stderr.contains("ERR_AGENTOS_WASM_TRAP"), "stderr={stderr}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "secondary trap did not reap the complete worker group"
    );

    start_threaded_process(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "process-after-thread-trap",
        &noop_entrypoint,
    );
    let (stdout, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-after-thread-trap",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 0, "stdout={stdout} stderr={stderr}");
}

#[test]
fn process_exit_and_wall_timeout_reap_threads_parked_in_atomic_wait() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    let name = "wasmtime-threaded-exit-timeout";
    let exit_module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (call $exit (i32.const 23))))"#,
    )
    .expect("threaded process-exit fixture");
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let exit_entrypoint = cwd.join("exit.wasm");
    let timeout_entrypoint = cwd.join("timeout.wasm");
    write_fixture(&exit_entrypoint, exit_module);
    write_fixture(&timeout_entrypoint, threaded_wait_forever_module());
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-exit");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([
            (String::from("limits.wasm.max_threads"), String::from("2")),
            (
                String::from("limits.wasm.wall_clock_limit_ms"),
                String::from("100"),
            ),
        ]),
    );

    let started = std::time::Instant::now();
    start_threaded_process(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-exit",
        &exit_entrypoint,
    );
    let (_, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-exit",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 23, "stderr={stderr}");
    assert!(started.elapsed() < Duration::from_secs(2));

    let started = std::time::Instant::now();
    start_threaded_process(
        &mut sidecar,
        5,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-timeout",
        &timeout_entrypoint,
    );
    let (_, stderr, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-timeout",
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 1, "stderr={stderr}");
    assert!(
        stderr.contains("ERR_AGENTOS_WASMTIME_WALL_CLOCK_LIMIT"),
        "stderr={stderr}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn vm_teardown_reaps_a_complete_atomic_wait_thread_group() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    let name = "wasmtime-threaded-vm-teardown";
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("wait.wasm");
    write_fixture(&entrypoint, threaded_wait_forever_module());
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-dispose");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
    );
    start_threaded_process(
        &mut sidecar,
        4,
        &connection_id,
        &session_id,
        &vm_id,
        "process-thread-dispose",
        &entrypoint,
    );
    std::thread::sleep(Duration::from_millis(200));
    let started = std::time::Instant::now();
    let disposed = sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::DisposeVmRequest(DisposeVmRequest {
                reason: DisposeReason::Requested,
            }),
        ))
        .expect("dispose VM with parked pthread group");
    assert!(matches!(
        disposed.response.payload,
        ResponsePayload::VmDisposedResponse(_)
    ));
    assert!(disposed.events.iter().any(|event| {
        matches!(
            &event.payload,
            EventPayload::ProcessExitedEvent(exited)
                if exited.process_id == "process-thread-dispose"
        )
    }));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "VM teardown exceeded the fixed threaded-worker reaping deadline"
    );
}

#[test]
fn concurrent_thread_groups_preserve_memory_isolation_and_process_admission() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    const GROUPS: usize = 8;
    let name = "wasmtime-threaded-high-concurrency";
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (drop (i32.atomic.rmw.add (i32.const 0) (i32.const 1)))
                (drop (memory.atomic.notify (i32.const 0) (i32.const 2))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1)) (then unreachable))
                (if (i32.lt_s (call $spawn (i32.const 2)) (i32.const 1)) (then unreachable))
                (loop $wait
                    (if (i32.lt_u (i32.atomic.load (i32.const 0)) (i32.const 2))
                        (then
                            (drop (memory.atomic.wait32
                                (i32.const 0)
                                (i32.atomic.load (i32.const 0))
                                (i64.const 100000000)))
                            (br $wait))))
                (if (i32.ne (i32.atomic.load (i32.const 0)) (i32.const 2))
                    (then unreachable))))"#,
    )
    .expect("high-concurrency pthread fixture");
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("concurrent.wasm");
    write_fixture(&entrypoint, module);
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-concurrency");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let mut processes = HashMap::new();
    for index in 0..GROUPS {
        let (vm_id, _) = support::create_vm_wire_with_metadata(
            &mut sidecar,
            10 + index as i64,
            &connection_id,
            &session_id,
            GuestRuntimeKind::WebAssembly,
            &cwd,
            HashMap::from([(String::from("limits.wasm.max_threads"), String::from("3"))]),
        );
        let process_id = format!("process-thread-concurrent-{index}");
        start_threaded_process(
            &mut sidecar,
            100 + index as i64,
            &connection_id,
            &session_id,
            &vm_id,
            &process_id,
            &entrypoint,
        );
        processes.insert(process_id, vm_id);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut exits = HashMap::new();
    while exits.len() < GROUPS && std::time::Instant::now() < deadline {
        let event = sidecar
            .poll_event_wire_blocking(
                &wire_session(&connection_id, &session_id),
                Duration::from_millis(100),
            )
            .expect("poll concurrent pthread events");
        if let Some(event) = event {
            if let EventPayload::ProcessExitedEvent(exited) = event.payload {
                if processes.contains_key(&exited.process_id) {
                    exits.insert(exited.process_id, exited.exit_code);
                }
            }
        }
    }
    assert_eq!(exits.len(), GROUPS, "not every threaded group completed");
    assert!(
        exits.values().all(|exit_code| *exit_code == 0),
        "threaded group failures: {exits:?}"
    );
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

#[test]
fn threaded_atomic_wait_is_killed_and_reaped_before_the_fixed_deadline() {
    std::env::set_var(
        "AGENTOS_WASMTIME_WORKER_PATH",
        env!("CARGO_BIN_EXE_agentos-native-sidecar"),
    );
    let name = "wasmtime-threaded-atomic-wait-kill";
    let module = wat::parse_str(
        r#"(module
            (import "env" "memory" (memory 1 2 shared))
            (import "wasi" "thread-spawn" (func $spawn (param i32) (result i32)))
            (export "memory" (memory 0))
            (func (export "wasi_thread_start") (param i32 i32)
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1))))
            (func (export "_start")
                (if (i32.lt_s (call $spawn (i32.const 1)) (i32.const 1))
                    (then unreachable))
                (drop (memory.atomic.wait32
                    (i32.const 0) (i32.const 0) (i64.const -1)))))"#,
    )
    .expect("atomic-wait fixture");
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("atomic-wait.wasm");
    write_fixture(&entrypoint, &module);
    let connection_id = authenticate_wire(&mut sidecar, "conn-wasmtime-thread-kill");
    let session_id = open_session_wire(&mut sidecar, 2, &connection_id);
    let (vm_id, _) = support::create_vm_wire_with_metadata(
        &mut sidecar,
        3,
        &connection_id,
        &session_id,
        GuestRuntimeKind::WebAssembly,
        &cwd,
        HashMap::from([(String::from("limits.wasm.max_threads"), String::from("2"))]),
    );
    let process_id = String::from("process-wasmtime-atomic-wait");
    sidecar
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
                wasm_permission_tier: Some(WasmPermissionTier::Full),
                wasm_backend: Some(StandaloneWasmBackend::WasmtimeThreads),
            }),
        ))
        .expect("start atomic-wait worker");
    std::thread::sleep(Duration::from_millis(200));
    let started = std::time::Instant::now();
    sidecar
        .dispatch_wire_blocking(wire_request(
            5,
            wire_vm(&connection_id, &session_id, &vm_id),
            RequestPayload::KillProcessRequest(KillProcessRequest {
                process_id: process_id.clone(),
                signal: String::from("SIGKILL"),
            }),
        ))
        .expect("kill atomic-wait worker");
    let (_, _, exit_code) = collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(5),
    );
    assert_eq!(exit_code, 137);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "atomic-wait worker exceeded the fixed reaping deadline"
    );
}
