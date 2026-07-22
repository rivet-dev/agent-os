mod support;

use agentos_native_sidecar::wire::{
    ExecuteRequest, GuestRuntimeKind, RequestPayload, ResponsePayload, StandaloneWasmBackend,
    WasmPermissionTier,
};
use agentos_wasm_abi_generator::{
    imports_module, raw_call_assertion_module, single_import_module, AbiImport, AbiManifest,
    CallArguments, RawCallAssertion,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;
use support::{
    assert_node_available, authenticate_wire, collect_process_output_wire_with_timeout,
    new_sidecar, open_session_wire, temp_dir, wire_request, wire_vm, write_fixture,
};

const ABI_MANIFEST: &str = include_str!("../../execution/assets/agentos-wasm-abi.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemoryContractObligation {
    InvalidInputRange,
    WrappedRange,
    InvalidOutputRange,
    AggregateCollectionBound,
    ShortOutput,
    AtomicCopyout,
    NoSideEffect,
}

/// A manifest import whose signature and behavior witness one or more raw-memory
/// obligations. More than one witness may share a signature because identical
/// WebAssembly types can have different semantic directions (for example,
/// scalar inputs, an input byte range, or two output pointers).
struct MemoryContractWitness {
    module: &'static str,
    name: &'static str,
    obligations: &'static [MemoryContractObligation],
}

const MEMORY_CONTRACT_WITNESSES: &[MemoryContractWitness] = &[
    MemoryContractWitness {
        module: "host_fs",
        name: "remount",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "fd_write",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::AggregateCollectionBound,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_user",
        name: "getpwuid",
        obligations: &[
            MemoryContractObligation::ShortOutput,
            MemoryContractObligation::AtomicCopyout,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "fd_getxattr",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "path_removexattr",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "random_get",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "fd_pipe",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_user",
        name: "setgroups",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::AggregateCollectionBound,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_user",
        name: "getresuid",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
        ],
    },
    MemoryContractWitness {
        module: "host_tty",
        name: "set_attr",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "fd_socketpair",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_net",
        name: "net_send",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_net",
        name: "net_recv",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_user",
        name: "getuid",
        obligations: &[MemoryContractObligation::InvalidOutputRange],
    },
    // This is the sole memory-bearing signature with an i64 result and is
    // exercised by `raw_i64_path_input_contract_module` below.
    MemoryContractWitness {
        module: "host_fs",
        name: "path_size",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "path_getxattr",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "path_setxattr",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_net",
        name: "net_getaddrinfo",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "fd_sendmsg_rights",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::AggregateCollectionBound,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "path_mknod",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_fs",
        name: "path_statfs",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
        ],
    },
    MemoryContractWitness {
        module: "host_net",
        name: "net_dns_query_rr_v1",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "fd_record_lock",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_itimer_real",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_ppoll_v1",
        obligations: &[
            MemoryContractObligation::AggregateCollectionBound,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_spawn",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_spawn_v2",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_spawn_v3",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "host_process",
        name: "proc_spawn_v4",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "clock_time_get",
        obligations: &[MemoryContractObligation::InvalidOutputRange],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "fd_pread",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "fd_seek",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "path_filestat_set_times",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "path_open",
        obligations: &[
            MemoryContractObligation::InvalidOutputRange,
            MemoryContractObligation::AtomicCopyout,
            MemoryContractObligation::NoSideEffect,
        ],
    },
    MemoryContractWitness {
        module: "wasi_snapshot_preview1",
        name: "path_open",
        obligations: &[
            MemoryContractObligation::InvalidInputRange,
            MemoryContractObligation::WrappedRange,
            MemoryContractObligation::NoSideEffect,
        ],
    },
];

/// Representatives for the six raw signature shapes which carry no guest
/// memory. They remain covered by the all-import invocation test (and proc_exit
/// has its dedicated terminal-call test), but deliberately are not padded with
/// meaningless pointer assertions.
const NON_MEMORY_SIGNATURE_WITNESSES: &[(&str, &str)] = &[
    ("host_fs", "fd_size"),
    ("host_process", "proc_setrlimit"),
    ("host_fs", "fd_zero_range"),
    ("host_fs", "ftruncate"),
    ("wasi_snapshot_preview1", "proc_exit"),
    ("wasi_snapshot_preview1", "sched_yield"),
];

fn abi_signature(import: &AbiImport) -> String {
    format!(
        "({})->({})",
        import.params.join(","),
        import.results.join(",")
    )
}

fn manifest_import<'a>(manifest: &'a AbiManifest, module: &str, name: &str) -> &'a AbiImport {
    manifest
        .imports
        .iter()
        .find(|import| import.module == module && import.name == name)
        .unwrap_or_else(|| panic!("missing ABI import {module}.{name}"))
}

fn run_raw_module(name: &str, module: &[u8], tier: WasmPermissionTier) -> (String, String, i32) {
    run_raw_module_with_metadata(name, module, tier, HashMap::new())
}

fn run_raw_module_with_metadata(
    name: &str,
    module: &[u8],
    tier: WasmPermissionTier,
    metadata: HashMap<String, String>,
) -> (String, String, i32) {
    let v8 = run_raw_module_for_backend(
        &format!("{name}-v8"),
        module,
        tier,
        metadata.clone(),
        StandaloneWasmBackend::V8,
    );
    let wasmtime = run_raw_module_for_backend(
        &format!("{name}-wasmtime"),
        module,
        tier,
        metadata,
        StandaloneWasmBackend::Wasmtime,
    );
    assert_eq!(
        wasmtime, v8,
        "Wasmtime and V8-WASM raw ABI outcomes diverged for {name}"
    );
    wasmtime
}

fn run_raw_module_for_backend(
    name: &str,
    module: &[u8],
    tier: WasmPermissionTier,
    metadata: HashMap<String, String>,
    backend: StandaloneWasmBackend,
) -> (String, String, i32) {
    let mut sidecar = new_sidecar(name);
    let cwd = temp_dir(&format!("{name}-cwd"));
    let entrypoint = cwd.join("raw-abi.wasm");
    write_fixture(&entrypoint, module);
    let connection_id = authenticate_wire(&mut sidecar, "conn-raw-abi");
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
                wasm_permission_tier: Some(tier),
                wasm_backend: Some(backend),
            }),
        ))
        .expect("start generated raw-ABI fixture through sidecar");
    assert!(
        matches!(
            started.response.payload,
            ResponsePayload::ProcessStartedResponse(_)
        ),
        "unexpected raw-ABI start response: {:?}",
        started.response.payload
    );
    collect_process_output_wire_with_timeout(
        &mut sidecar,
        &connection_id,
        &session_id,
        &vm_id,
        &process_id,
        Duration::from_secs(30),
    )
}

#[test]
fn every_permitted_raw_abi_import_is_invoked_at_every_permission_tier() {
    assert_node_available();
    let manifest = AbiManifest::parse(ABI_MANIFEST);

    for (tier_name, tier) in [
        ("isolated", WasmPermissionTier::Isolated),
        ("read-only", WasmPermissionTier::ReadOnly),
        ("read-write", WasmPermissionTier::ReadWrite),
        ("full", WasmPermissionTier::Full),
    ] {
        // A fresh VM gives scalar state-mutating calls (credentials, umask,
        // fd flags) no opportunity to contaminate another permission tier.
        let permitted = manifest.permitted_imports(tier_name);
        assert!(!permitted.is_empty(), "{tier_name} ABI must not be empty");
        let (stdout, stderr, exit_code) = run_raw_module(
            &format!("wasm-raw-abi-{tier_name}"),
            &imports_module(&permitted, true, CallArguments::Hostile),
            tier,
        );
        assert_eq!(
            exit_code, 0,
            "{tier_name} raw ABI invocation failed: stdout={stdout} stderr={stderr}"
        );
    }
}

#[test]
fn preview1_proc_exit_and_compatibility_alias_execute_through_sidecar() {
    assert_node_available();
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let proc_exit = manifest
        .imports
        .iter()
        .find(|import| import.module == "wasi_snapshot_preview1" && import.name == "proc_exit")
        .expect("Preview1 proc_exit manifest entry");

    for module in ["wasi_snapshot_preview1", "wasi_unstable"] {
        let mut import = proc_exit.clone();
        import.module = module.to_string();
        let (stdout, stderr, exit_code) = run_raw_module(
            &format!("wasm-raw-abi-proc-exit-{module}"),
            &single_import_module(&import, true, CallArguments::Zero),
            WasmPermissionTier::Full,
        );
        assert_eq!(
            exit_code, 0,
            "{module}.proc_exit failed through sidecar: stdout={stdout} stderr={stderr}"
        );
    }
}

fn raw_abi_memory_contract_assertions() -> Vec<RawCallAssertion> {
    let i32c = |value: i64| format!("(i32.const {value})");
    let i64c = |value: i64| format!("(i64.const {value})");
    vec![
        // Fixed-size and multi-output destinations are prevalidated in full.
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "clock_time_get",
            [i32c(0), i64c(0), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_unstable",
            "clock_time_get",
            [i32c(0), i64c(0), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "path_statfs",
            [
                i32c(-1),
                i32c(0),
                i32c(0),
                i32c(296),
                i32c(304),
                i32c(312),
                i32c(320),
                i32c(65_532),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_socket",
            [i32c(0), i32c(0), i32c(0), i32c(65_534)],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_getrlimit",
            [i32c(7), i32c(272), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_getrlimit",
            [i32c(-1), i32c(616), i32c(624)],
            28,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_setrlimit",
            [i32c(-1), i64c(-1), i64c(-1)],
            28,
        ),
        RawCallAssertion::i32("host_process", "proc_kill", [i32c(-1), i32c(-1)], 28),
        RawCallAssertion::i32(
            "host_process",
            "proc_kill",
            [i32c(2_147_483_647), i32c(0)],
            71,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_sigaction",
            [i32c(-1), i32c(-1), i32c(-1), i32c(-1), i32c(-1)],
            28,
        ),
        // The owned raw signal ABI spans the complete two-word mask domain.
        // Ignore is deliverable without a guest trampoline; a user handler is
        // rejected when this raw fixture does not export one.
        RawCallAssertion::i32(
            "host_process",
            "proc_sigaction",
            [i32c(64), i32c(1), i32c(0), i32c(0), i32c(0)],
            0,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_sigaction",
            [i32c(65), i32c(1), i32c(0), i32c(0), i32c(0)],
            28,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_sigaction",
            [i32c(2), i32c(2), i32c(0), i32c(0), i32c(0)],
            58,
        ),
        RawCallAssertion::i32(
            "host_tty",
            "get_size",
            [i32c(-1), i32c(288), i32c(65_535)],
            21,
        ),
        RawCallAssertion::i32(
            "host_user",
            "getresuid",
            [i32c(280), i32c(284), i32c(65_534)],
            21,
        ),
        RawCallAssertion::i32(
            "host_system",
            "get_identity",
            [i32c(0), i32c(65_520), i32c(32)],
            21,
        ),
        RawCallAssertion::i32("host_process", "fd_pipe", [i32c(65_534), i32c(640)], 21),
        RawCallAssertion::i32(
            "host_process",
            "fd_socketpair",
            [i32c(0), i32c(0), i32c(0), i32c(65_534), i32c(640)],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "fd_getxattr",
            [
                i32c(-1),
                i32c(0),
                i32c(0),
                i32c(65_520),
                i32c(32),
                i32c(640),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "path_getxattr",
            [
                i32c(-1),
                i32c(0),
                i32c(1),
                i32c(1),
                i32c(1),
                i32c(65_520),
                i32c(32),
                i32c(0),
                i32c(640),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_getaddrinfo",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_itimer_real",
            [i32c(0), i64c(0), i64c(0), i32c(640), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "fd_seek",
            [i32c(-1), i64c(0), i32c(0), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "path_open",
            [
                i32c(3),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i64c(0),
                i64c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        // Pointer+length wrap and OOB input ranges never reach the resource.
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "random_get",
            [i32c(65_520), i32c(32)],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "remount",
            [i32c(65_520), i32c(32), i32c(0), i32c(0)],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "path_removexattr",
            [i32c(-1), i32c(65_520), i32c(32), i32c(0), i32c(0), i32c(0)],
            21,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_send",
            [i32c(-1), i32c(65_520), i32c(32), i32c(0), i32c(640)],
            21,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_recv",
            [i32c(-1), i32c(65_520), i32c(32), i32c(0), i32c(640)],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "path_setxattr",
            [
                i32c(3),
                i32c(0),
                i32c(1),
                i32c(1),
                i32c(1),
                i32c(65_520),
                i32c(32),
                i32c(0),
                i32c(0),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "fd_sendmsg_rights",
            [
                i32c(-1),
                i32c(65_520),
                i32c(32),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(640),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_dns_query_rr_v1",
            [
                i32c(65_520),
                i32c(32),
                i32c(12),
                i32c(1_024),
                i32c(64),
                i32c(1_088),
                i32c(1_092),
                i32c(1_096),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_fs",
            "path_mknod",
            [i32c(-1), i32c(65_520), i32c(32), i32c(0), i64c(0)],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "fd_pread",
            [i32c(-1), i32c(65_528), i32c(2), i64c(0), i32c(640)],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "path_filestat_set_times",
            [
                i32c(-1),
                i32c(0),
                i32c(65_520),
                i32c(32),
                i64c(0),
                i64c(0),
                i32c(0),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "path_open",
            [
                i32c(3),
                i32c(0),
                i32c(65_520),
                i32c(32),
                i32c(0),
                i64c(0),
                i64c(0),
                i32c(0),
                i32c(640),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn_v4",
            [
                i32c(65_520),
                i32c(32),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(512),
            ],
            21,
        ),
        // Every spawn ABI prevalidates its result pointer. The following
        // wait-any probe then proves that none of the rejected calls created a
        // child before discovering the bad copyout range.
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(1),
                i32c(2),
                i32c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn_v2",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(1),
                i32c(2),
                i32c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn_v3",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn_v4",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(65_534),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_waitpid",
            [i32c(-1), i32c(1), i32c(640), i32c(644)],
            12,
        ),
        RawCallAssertion::i32(
            "host_tty",
            "set_attr",
            [i32c(-1), i32c(0), i32c(65_532)],
            21,
        ),
        RawCallAssertion::i32("host_tty", "set_size", [i32c(-1), i32c(-1), i32c(-1)], 28),
        RawCallAssertion::i32("host_user", "setgroups", [i32c(1), i32c(65_534)], 21),
        // Counts and decoded collections are rejected before reading tables.
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "fd_write",
            [i32c(1), i32c(0), i32c(1_025), i32c(512)],
            28,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "poll_oneoff",
            [i32c(0), i32c(0), i32c(1_025), i32c(512)],
            28,
        ),
        RawCallAssertion::i32(
            "host_net",
            "net_poll",
            [i32c(0), i32c(-1), i32c(0), i32c(512)],
            28,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_ppoll_v1",
            [
                i32c(0),
                i32c(-1),
                i64c(0),
                i64c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(512),
            ],
            28,
        ),
        RawCallAssertion::i32("host_user", "setgroups", [i32c(65), i32c(65_535)], 28),
        RawCallAssertion::i32(
            "host_user",
            "getpwnam",
            [i32c(65_535), i32c(4_097), i32c(1_024), i32c(0), i32c(512)],
            37,
        ),
        RawCallAssertion::i32(
            "host_process",
            "proc_spawn_v4",
            [
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(1_048_577),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(0),
                i32c(512),
            ],
            1,
        ),
        // Open the fixture itself so F_GETLK reaches its four-output copyout.
        // F_GETLK is read-only; the bad first result pointer must leave every
        // later result sentinel untouched.
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "path_open",
            [
                i32c(3),
                i32c(0),
                i32c(700),
                i32c(12),
                i32c(0),
                i64c(0),
                i64c(0),
                i32c(0),
                i32c(680),
            ],
            0,
        ),
        RawCallAssertion::i32(
            "host_process",
            "fd_record_lock",
            [
                String::from("(i32.load (i32.const 680))"),
                i32c(12),
                i32c(0),
                i64c(0),
                i64c(0),
                i32c(65_534),
                i32c(640),
                i32c(648),
                i32c(656),
            ],
            21,
        ),
        RawCallAssertion::i32(
            "wasi_snapshot_preview1",
            "fd_close",
            [String::from("(i32.load (i32.const 680))")],
            0,
        ),
        // Short account buffers publish required length without partial data.
        RawCallAssertion::i32("host_user", "getuid", [i32c(65_534)], 21),
        RawCallAssertion::i32("host_user", "getuid", [i32c(600)], 0),
        RawCallAssertion::i32(
            "host_user",
            "getpwuid",
            [
                String::from("(i32.load (i32.const 600))"),
                i32c(1_024),
                i32c(0),
                i32c(512),
            ],
            68,
        ),
        RawCallAssertion::i32(
            "host_system",
            "get_identity",
            [i32c(0), i32c(1_024), i32c(1)],
            37,
        ),
    ]
}

fn raw_i64_path_input_contract_module(manifest: &AbiManifest) -> Vec<u8> {
    let path_size = manifest_import(manifest, "host_fs", "path_size");
    assert_eq!(
        abi_signature(path_size),
        "(i32,i32,i32,i32)->(i64)",
        "path_size raw-memory witness changed signature"
    );
    let proc_exit = manifest_import(manifest, "wasi_snapshot_preview1", "proc_exit");
    assert_eq!(abi_signature(proc_exit), "(i32)->()");

    wat::parse_str(
        r#"
(module
  (type $path_size_t (func (param i32 i32 i32 i32) (result i64)))
  (type $proc_exit_t (func (param i32)))
  (import "host_fs" "path_size" (func $path_size (type $path_size_t)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (type $proc_exit_t)))
  (memory (export "memory") 1)
  (func (export "_start")
    ;; The wrapped path range must return the ABI's i64 error sentinel without
    ;; consulting the filesystem or trapping the guest.
    (if
      (i64.ne
        (call $path_size
          (i32.const 3)
          (i32.const 65520)
          (i32.const 32)
          (i32.const 0)
        )
        (i64.const -1)
      )
      (then (call $proc_exit (i32.const 1)) unreachable)
    )
  )
)
"#,
    )
    .expect("compile i64 raw-memory contract module")
}

fn raw_fixed_limit_family_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "poll_oneoff" (func $poll_oneoff (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (import "host_net" "net_poll" (func $net_poll (param i32 i32 i32 i32) (result i32)))
  (import "host_user" "setgroups" (func $setgroups (param i32 i32) (result i32)))
  (import "host_user" "getpwnam" (func $getpwnam (param i32 i32 i32 i32 i32) (result i32)))
  (import "host_fs" "path_setxattr" (func $path_setxattr (param i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 4)
  (data (i32.const 120000) "missing")
  (func $fail (param $code i32)
    (call $proc_exit (local.get $code))
    unreachable
  )
  (func (export "_start") (local $index i32) (local $result i32)
    ;; A 4096-byte unknown account name and a 255-byte valid xattr name.
    (memory.fill (i32.const 110000) (i32.const 97) (i32.const 4096))
    (memory.fill (i32.const 120016) (i32.const 97) (i32.const 256))
    (i32.store (i32.const 120016) (i32.const 1919251317)) ;; "user"
    (i32.store8 (i32.const 120020) (i32.const 46))       ;; "."

    ;; Build 1024 inert pollfds and 64 valid supplementary group IDs.
    (local.set $index (i32.const 0))
    (block $poll_done
      (loop $poll_fill
        (br_if $poll_done (i32.ge_u (local.get $index) (i32.const 1024)))
        (i32.store
          (i32.add (i32.const 8192) (i32.mul (local.get $index) (i32.const 8)))
          (i32.const -1)
        )
        (local.set $index (i32.add (local.get $index) (i32.const 1)))
        (br $poll_fill)
      )
    )
    (local.set $index (i32.const 0))
    (block $groups_done
      (loop $groups_fill
        (br_if $groups_done (i32.ge_u (local.get $index) (i32.const 64)))
        (i32.store
          (i32.add (i32.const 100000) (i32.mul (local.get $index) (i32.const 4)))
          (local.get $index)
        )
        (local.set $index (i32.add (local.get $index) (i32.const 1)))
        (br $groups_fill)
      )
    )

    ;; Exact fixed-table boundaries are accepted; limit+1 is rejected before
    ;; any table walk. Zero-filled subscriptions are immediate clock waits.
    (if (i32.ne (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1024) (i32.const 18000)) (i32.const 0))
      (then (call $fail (i32.const 201))))
    (if (i32.ne (call $fd_write (i32.const 1) (i32.const 0) (i32.const 1025) (i32.const 18000)) (i32.const 28))
      (then (call $fail (i32.const 202))))
    (if (i32.ne (call $net_poll (i32.const 8192) (i32.const 1024) (i32.const 0) (i32.const 18004)) (i32.const 0))
      (then (call $fail (i32.const 203))))
    (if (i32.ne (call $net_poll (i32.const 8192) (i32.const 1025) (i32.const 0) (i32.const 18004)) (i32.const 28))
      (then (call $fail (i32.const 204))))
    (if (i32.ne (call $poll_oneoff (i32.const 20000) (i32.const 70000) (i32.const 1024) (i32.const 103000)) (i32.const 0))
      (then (call $fail (i32.const 205))))
    (if (i32.ne (call $poll_oneoff (i32.const 20000) (i32.const 70000) (i32.const 1025) (i32.const 103000)) (i32.const 28))
      (then (call $fail (i32.const 206))))

    ;; Fixed list/string/value caps use the same boundary and warning contract.
    ;; The VM runs as a non-root identity, so the exact setgroups boundary must
    ;; reach the kernel and return EPERM (63); the decoder rejects limit+1 as
    ;; EINVAL (28) before that permission check.
    (if (i32.ne (call $setgroups (i32.const 64) (i32.const 100000)) (i32.const 63))
      (then (call $fail (i32.const 207))))
    (if (i32.ne (call $setgroups (i32.const 65) (i32.const 100000)) (i32.const 28))
      (then (call $fail (i32.const 208))))
    (if (i32.ne (call $getpwnam (i32.const 110000) (i32.const 4096) (i32.const 0) (i32.const 0) (i32.const 115000)) (i32.const 44))
      (then (call $fail (i32.const 209))))
    (if (i32.ne (call $getpwnam (i32.const 110000) (i32.const 4097) (i32.const 0) (i32.const 0) (i32.const 115000)) (i32.const 37))
      (then (call $fail (i32.const 210))))
    (local.set $result
      (call $path_setxattr
        (i32.const -1) (i32.const 120000) (i32.const 7)
        (i32.const 120016) (i32.const 255)
        (i32.const 131072) (i32.const 65536)
        (i32.const 0) (i32.const 1)))
    (if (i32.ne (local.get $result) (i32.const 44))
      (then (call $fail (i32.const 211))))
    (if (i32.ne
      (call $path_setxattr
        (i32.const -1) (i32.const 120000) (i32.const 7)
        (i32.const 120016) (i32.const 255)
        (i32.const 131072) (i32.const 65537)
        (i32.const 0) (i32.const 1))
      (i32.const 1))
      (then (call $fail (i32.const 212))))
    (if (i32.ne
      (call $path_setxattr
        (i32.const -1) (i32.const 120000) (i32.const 7)
        (i32.const 120016) (i32.const 256)
        (i32.const 131072) (i32.const 0)
        (i32.const 0) (i32.const 1))
      (i32.const 28))
      (then (call $fail (i32.const 213))))
  )
)
"#,
    )
    .expect("compile fixed-limit family module")
}

fn raw_blocking_read_deadline_module() -> Vec<u8> {
    wat::parse_str(
        r#"
(module
  (import "host_net" "net_poll" (func $net_poll (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (func (export "_start")
    (if
      (i32.ne
        (call $net_poll (i32.const 0) (i32.const 0) (i32.const -1) (i32.const 16))
        (i32.const 73)
      )
      (then (call $proc_exit (i32.const 221)) unreachable)
    )
  )
)
"#,
    )
    .expect("compile blocking-read deadline module")
}

#[test]
fn raw_abi_manifest_signature_families_have_auditable_memory_contracts() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let manifest_signatures = manifest
        .imports
        .iter()
        .map(abi_signature)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        manifest_signatures.len(),
        29,
        "review every new raw signature shape and classify its guest-memory contract"
    );

    let assertion_imports = manifest.imports_with_aliases();
    let assertion_names = raw_abi_memory_contract_assertions()
        .into_iter()
        .map(|assertion| (assertion.module, assertion.name))
        .collect::<BTreeSet<_>>();
    let mut memory_signatures = BTreeSet::new();
    let mut obligation_witnesses = BTreeMap::<MemoryContractObligation, Vec<String>>::new();
    for witness in MEMORY_CONTRACT_WITNESSES {
        let import = assertion_imports
            .iter()
            .find(|import| import.module == witness.module && import.name == witness.name)
            .unwrap_or_else(|| {
                panic!(
                    "raw-memory witness references missing import {}.{}",
                    witness.module, witness.name
                )
            });
        memory_signatures.insert(abi_signature(import));
        if import.results.as_slice() == ["i32"] {
            assert!(
                assertion_names.contains(&(witness.module.to_owned(), witness.name.to_owned())),
                "memory witness {}.{} must execute in the hostile assertion module",
                witness.module,
                witness.name
            );
        } else {
            assert_eq!(
                (witness.module, witness.name),
                ("host_fs", "path_size"),
                "only path_size uses the separately checked i64 memory fixture"
            );
        }
        assert!(
            !witness.obligations.is_empty(),
            "memory witness {}.{} must name its semantic obligation",
            witness.module,
            witness.name
        );
        for obligation in witness.obligations {
            obligation_witnesses
                .entry(*obligation)
                .or_default()
                .push(format!("{}.{}", witness.module, witness.name));
        }
    }

    let non_memory_signatures = NON_MEMORY_SIGNATURE_WITNESSES
        .iter()
        .map(|(module, name)| abi_signature(manifest_import(&manifest, module, name)))
        .collect::<BTreeSet<_>>();
    assert_eq!(memory_signatures.len(), 23);
    assert_eq!(non_memory_signatures.len(), 6);
    assert!(
        memory_signatures.is_disjoint(&non_memory_signatures),
        "a signature shape cannot be both memory-bearing and scalar-only"
    );
    assert_eq!(
        memory_signatures
            .union(&non_memory_signatures)
            .cloned()
            .collect::<BTreeSet<_>>(),
        manifest_signatures,
        "every generated-manifest signature must have an explicit raw-memory or non-memory contract"
    );

    for obligation in [
        MemoryContractObligation::InvalidInputRange,
        MemoryContractObligation::WrappedRange,
        MemoryContractObligation::InvalidOutputRange,
        MemoryContractObligation::AggregateCollectionBound,
        MemoryContractObligation::ShortOutput,
        MemoryContractObligation::AtomicCopyout,
        MemoryContractObligation::NoSideEffect,
    ] {
        assert!(
            obligation_witnesses.contains_key(&obligation),
            "missing raw-memory witness for {obligation:?}"
        );
    }
}

#[test]
fn raw_abi_memory_directions_reject_hostile_ranges_before_host_work() {
    assert_node_available();
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let assertions = raw_abi_memory_contract_assertions();
    let setup = r#"
    (i64.store (i32.const 272) (i64.const 1234605616436508552))
    (i32.store (i32.const 280) (i32.const 287454020))
    (i32.store (i32.const 284) (i32.const 1432778632))
    (i32.store (i32.const 288) (i32.const 16909060))
    (i64.store (i32.const 296) (i64.const 72623859790382856))
    (i32.store (i32.const 512) (i32.const 0))
    (i64.store (i32.const 616) (i64.const 1084818905618843912))
    (i64.store (i32.const 624) (i64.const 506097522914230528))
    (i32.store (i32.const 640) (i32.const 287454020))
    (i32.store (i32.const 644) (i32.const 1432778632))
    (i64.store (i32.const 648) (i64.const 72623859790382856))
    (i64.store (i32.const 656) (i64.const 1230066625199609624))
    (i64.store (i32.const 700) (i64.const 3344312367813452146))
    (i64.store (i32.const 708) (i64.const 1836278135))
    (i64.store (i32.const 1024) (i64.const 1230066625199609624))
"#;
    let postconditions = r#"
    (if (i64.ne (i64.load (i32.const 272)) (i64.const 1234605616436508552)) (then (call $assert_fail (i32.const 101)) unreachable))
    (if (i32.ne (i32.load (i32.const 280)) (i32.const 287454020)) (then (call $assert_fail (i32.const 102)) unreachable))
    (if (i32.ne (i32.load (i32.const 284)) (i32.const 1432778632)) (then (call $assert_fail (i32.const 103)) unreachable))
    (if (i32.ne (i32.load (i32.const 288)) (i32.const 16909060)) (then (call $assert_fail (i32.const 104)) unreachable))
    (if (i64.ne (i64.load (i32.const 296)) (i64.const 72623859790382856)) (then (call $assert_fail (i32.const 105)) unreachable))
    (if (i32.eqz (i32.load (i32.const 512))) (then (call $assert_fail (i32.const 106)) unreachable))
    (if (i64.ne (i64.load (i32.const 616)) (i64.const 1084818905618843912)) (then (call $assert_fail (i32.const 107)) unreachable))
    (if (i64.ne (i64.load (i32.const 624)) (i64.const 506097522914230528)) (then (call $assert_fail (i32.const 108)) unreachable))
    (if (i32.ne (i32.load (i32.const 640)) (i32.const 287454020)) (then (call $assert_fail (i32.const 109)) unreachable))
    (if (i32.ne (i32.load (i32.const 644)) (i32.const 1432778632)) (then (call $assert_fail (i32.const 110)) unreachable))
    (if (i64.ne (i64.load (i32.const 648)) (i64.const 72623859790382856)) (then (call $assert_fail (i32.const 111)) unreachable))
    (if (i64.ne (i64.load (i32.const 656)) (i64.const 1230066625199609624)) (then (call $assert_fail (i32.const 112)) unreachable))
    (if (i64.ne (i64.load (i32.const 1024)) (i64.const 1230066625199609624)) (then (call $assert_fail (i32.const 113)) unreachable))
"#;
    let module = raw_call_assertion_module(&manifest, &assertions, setup, postconditions);
    let (stdout, stderr, exit_code) =
        run_raw_module("wasm-raw-abi-memory", &module, WasmPermissionTier::Full);
    assert_eq!(
        exit_code, 0,
        "raw ABI memory proof failed: stdout={stdout} stderr={stderr}"
    );

    let i64_module = raw_i64_path_input_contract_module(&manifest);
    let (stdout, stderr, exit_code) = run_raw_module(
        "wasm-raw-abi-memory-i64",
        &i64_module,
        WasmPermissionTier::Full,
    );
    assert_eq!(
        exit_code, 0,
        "raw ABI i64 memory proof failed: stdout={stdout} stderr={stderr}"
    );
}

#[test]
fn raw_abi_fixed_tables_lists_and_strings_prove_boundary_plus_one_and_warning() {
    assert_node_available();
    let module = raw_fixed_limit_family_module();
    let (stdout, stderr, exit_code) = run_raw_module_with_metadata(
        "wasm-raw-abi-fixed-limits",
        &module,
        WasmPermissionTier::Full,
        HashMap::from([(String::from("resource.max_open_fds"), String::from("1024"))]),
    );
    assert_eq!(
        exit_code, 0,
        "raw ABI fixed-limit proof failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.is_empty(),
        "fixed-limit probes must not write stdout"
    );
    for limit_name in [
        "wasm.abi.maxIovecs",
        "wasm.abi.maxPollFds",
        "wasm.abi.maxPollSubscriptions",
        "wasm.abi.maxSupplementaryGroups",
        "wasm.abi.maxAccountNameBytes",
        "wasm.abi.maxXattrNameBytes",
        "wasm.abi.maxXattrValueBytes",
    ] {
        assert!(
            stderr.contains(limit_name),
            "missing 80% boundary warning for {limit_name}: {stderr}"
        );
    }
}

#[test]
fn raw_abi_blocking_read_warns_at_eighty_percent_before_typed_expiry() {
    assert_node_available();
    let module = raw_blocking_read_deadline_module();
    let (stdout, stderr, exit_code) = run_raw_module_with_metadata(
        "wasm-raw-abi-blocking-deadline",
        &module,
        WasmPermissionTier::Full,
        HashMap::from([(
            String::from("resource.max_blocking_read_ms"),
            String::from("50"),
        )]),
    );
    assert_eq!(
        exit_code, 0,
        "raw ABI blocking-read deadline proof failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("blocking poll is nearing limits.resources.maxBlockingReadMs (50 ms)"),
        "80% warning must precede the typed timeout: {stderr}"
    );
    assert!(
        stderr.contains("blocking poll exceeded limits.resources.maxBlockingReadMs (50 ms)"),
        "hard expiry must retain the configured setting: {stderr}"
    );
}

#[test]
fn known_unsupported_preview1_imports_fail_closed() {
    assert_node_available();
    for module in ["wasi_snapshot_preview1", "wasi_unstable"] {
        for (name, params) in [
            ("fd_advise", vec!["i32", "i64", "i64", "i32"]),
            ("fd_fdstat_set_rights", vec!["i32", "i64", "i64"]),
        ] {
            let import = AbiImport {
                module: module.to_string(),
                name: name.to_string(),
                params: params.into_iter().map(String::from).collect(),
                results: vec![String::from("i32")],
            };
            let (stdout, stderr, exit_code) = run_raw_module(
                &format!("wasm-raw-unsupported-{module}-{name}"),
                &single_import_module(&import, false, CallArguments::Zero),
                WasmPermissionTier::Full,
            );
            assert_ne!(
                exit_code, 0,
                "unsupported {}.{} linked: stdout={stdout} stderr={stderr}",
                import.module, import.name
            );
            assert!(
                stderr.contains(&import.name) || stderr.contains(&import.module),
                "unexpected unsupported-import error for {}.{}: {stderr}",
                import.module,
                import.name
            );
        }
    }
}
