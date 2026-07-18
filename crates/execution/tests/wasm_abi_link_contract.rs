mod support;

use agentos_execution::{CreateWasmContextRequest, StartWasmExecutionRequest, WasmPermissionTier};
use agentos_wasm_abi_generator::{
    imports_module, single_import_module, AbiImport, AbiManifest, CallArguments,
};
use std::{collections::BTreeMap, fs};
use tempfile::tempdir;

const ABI_MANIFEST: &str = include_str!("../assets/agentos-wasm-abi.json");

fn run_fixture(
    engine: &mut agentos_execution::WasmExecutionEngine,
    root: &std::path::Path,
    file_name: &str,
    bytes: &[u8],
    tier: WasmPermissionTier,
) -> agentos_execution::WasmExecutionResult {
    fs::write(root.join(file_name), bytes).expect("write generated ABI fixture");
    let context = engine.create_context(CreateWasmContextRequest {
        vm_id: String::from("vm-wasm-abi-link"),
        module_path: Some(format!("./{file_name}")),
    });
    engine
        .start_execution(StartWasmExecutionRequest {
            guest_runtime: Default::default(),
            limits: Default::default(),
            vm_id: String::from("vm-wasm-abi-link"),
            context_id: context.context_id,
            managed_kernel_host: false,
            argv: Vec::new(),
            env: BTreeMap::new(),
            cwd: root.to_path_buf(),
            permission_tier: tier,
        })
        .expect("start generated ABI fixture")
        .wait()
        .expect("wait for generated ABI fixture")
}

#[test]
fn every_permitted_import_and_preview1_alias_links_at_every_tier() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    for (tier_name, tier) in [
        ("isolated", WasmPermissionTier::Isolated),
        ("read-only", WasmPermissionTier::ReadOnly),
        ("read-write", WasmPermissionTier::ReadWrite),
        ("full", WasmPermissionTier::Full),
    ] {
        let permitted = manifest.permitted_imports(tier_name);
        assert!(!permitted.is_empty(), "{tier_name} ABI must not be empty");
        let result = run_fixture(
            &mut engine,
            temp.path(),
            &format!("linkable-{tier_name}.wasm"),
            &imports_module(&permitted, false, CallArguments::Zero),
            tier,
        );
        assert_eq!(
            result.exit_code,
            0,
            "{tier_name} ABI failed to link: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn preview1_proc_exit_and_compatibility_alias_are_terminal_calls() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let proc_exit = manifest
        .imports
        .iter()
        .find(|import| import.module == "wasi_snapshot_preview1" && import.name == "proc_exit")
        .expect("Preview1 proc_exit manifest entry");
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    for module in ["wasi_snapshot_preview1", "wasi_unstable"] {
        let mut import = proc_exit.clone();
        import.module = module.to_string();
        let result = run_fixture(
            &mut engine,
            temp.path(),
            &format!("proc-exit-{module}.wasm"),
            &single_import_module(&import, true, CallArguments::Zero),
            WasmPermissionTier::Full,
        );
        assert_eq!(
            result.exit_code,
            0,
            "{module}.proc_exit did not execute: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn manifest_permission_tiers_omit_denied_and_undeclared_imports() {
    let manifest = AbiManifest::parse(ABI_MANIFEST);
    let temp = tempdir().expect("create temp dir");
    let mut engine = support::wasm_engine();

    let cases = [
        ("host_net", "net_socket", WasmPermissionTier::ReadWrite),
        (
            "host_process",
            "proc_spawn_v4",
            WasmPermissionTier::ReadWrite,
        ),
        ("host_process", "fd_getfd", WasmPermissionTier::Isolated),
    ];
    for (module, name, tier) in cases {
        let import = manifest
            .imports
            .iter()
            .find(|import| import.module == module && import.name == name)
            .unwrap_or_else(|| panic!("missing {module}.{name} manifest entry"));
        let result = run_fixture(
            &mut engine,
            temp.path(),
            &format!("denied-{module}-{name}.wasm"),
            &single_import_module(import, false, CallArguments::Zero),
            tier,
        );
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert_ne!(
            result.exit_code, 0,
            "{module}.{name} must not link at {tier:?}"
        );
        assert!(
            stderr.contains(module) || stderr.contains(name),
            "unexpected denied-import error for {module}.{name}: {stderr}"
        );
    }

    let undeclared = AbiImport {
        module: String::from("host_unknown"),
        name: String::from("ambient_escape"),
        params: Vec::new(),
        results: Vec::new(),
    };
    let rejected = run_fixture(
        &mut engine,
        temp.path(),
        "undeclared-import.wasm",
        &single_import_module(&undeclared, false, CallArguments::Zero),
        WasmPermissionTier::Full,
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_ne!(rejected.exit_code, 0, "undeclared import must not link");
    assert!(
        stderr.contains("host_unknown") || stderr.contains("ambient_escape"),
        "unexpected undeclared-import error: {stderr}"
    );
}
