//! Shared e2e helpers: resolve/point at the real `agentos-sidecar` binary and build VMs.
//!
//! Resolve order for the binary: `AGENTOS_SIDECAR_BIN`, then `CARGO_TARGET_DIR`, else
//! `<workspace>/target/debug/agentos-sidecar`.
//! Build it first with `cargo build -p agentos-sidecar`.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Once;

use agentos_client::config::{
    node_modules_mount, AgentOsConfig, AgentOsSidecarConfig, MountConfig, MountPlugin, PackageRef,
    Permissions,
};
use agentos_client::AgentOs;

static INIT: Once = Once::new();

fn test_node_modules_dir() -> PathBuf {
    std::env::var_os("AGENTOS_TEST_NODE_MODULES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../node_modules"))
}

pub fn ensure_sidecar_env() {
    INIT.call_once(|| {
        if std::env::var("AGENTOS_SIDECAR_BIN").is_err() {
            let target_dir = std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                            .join("../..")
                            .join(path)
                    }
                })
                .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
            let bin = target_dir.join("debug/agentos-sidecar");
            // `std::env::set_var` is `unsafe` in the Rust 2024 edition (process-global mutation that
            // can race other threads reading the environment). This runs once, single-threaded, under
            // `Once::call_once` before any VM is created. The `allow` keeps it warning-free on the
            // 2021 edition, where the call is still safe.
            #[allow(unused_unsafe)]
            unsafe {
                std::env::set_var("AGENTOS_SIDECAR_BIN", bin);
            }
        }
    });
}

/// Whether the sidecar binary is present.
pub fn sidecar_available() -> bool {
    ensure_sidecar_env();
    std::env::var("AGENTOS_SIDECAR_BIN")
        .map(|path| PathBuf::from(path).exists())
        .unwrap_or(false)
}

pub fn allow_local_e2e_skips() -> bool {
    std::env::var("AGENT_OS_CLIENT_ALLOW_E2E_SKIPS")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn require_sidecar(test_name: &str) -> bool {
    if sidecar_available() {
        return true;
    }

    let message = format!("{test_name}: sidecar binary is not built");
    if allow_local_e2e_skips() {
        eprintln!("skipping {message}");
        false
    } else {
        panic!("{message}; build it with `cargo build -p agentos-sidecar` or set AGENT_OS_CLIENT_ALLOW_E2E_SKIPS=1 for local skip-only runs");
    }
}

/// Create a VM with default config against the real sidecar.
pub async fn new_vm() -> AgentOs {
    new_vm_with_loopback_ports(Vec::new()).await
}

pub async fn new_vm_with_sidecar_pool(pool: impl Into<String>) -> AgentOs {
    ensure_sidecar_env();
    AgentOs::create(AgentOsConfig {
        mounts: vec![node_modules_mount(
            test_node_modules_dir().to_string_lossy().into_owned(),
        )],
        sidecar: Some(AgentOsSidecarConfig::Shared {
            pool: Some(pool.into()),
        }),
        ..Default::default()
    })
    .await
    .expect("create VM against real sidecar")
}

pub async fn new_vm_with_loopback_ports(loopback_exempt_ports: Vec<u16>) -> AgentOs {
    new_vm_with_config(loopback_exempt_ports, Vec::new(), None).await
}

pub async fn new_vm_with_wasm_commands() -> AgentOs {
    new_vm_with_wasm_commands_and_loopback_ports(Vec::new()).await
}

pub async fn new_vm_with_wasm_commands_and_loopback_ports(
    loopback_exempt_ports: Vec<u16>,
) -> AgentOs {
    new_vm_with_config(loopback_exempt_ports, wasm_command_mounts(), None).await
}

pub async fn new_vm_with_wasm_commands_and_permissions(permissions: Permissions) -> AgentOs {
    new_vm_with_config(Vec::new(), wasm_command_mounts(), Some(permissions)).await
}

async fn new_vm_with_config(
    loopback_exempt_ports: Vec<u16>,
    mounts: Vec<MountConfig>,
    permissions: Option<Permissions>,
) -> AgentOs {
    ensure_sidecar_env();
    let mut all_mounts = vec![node_modules_mount(
        test_node_modules_dir().to_string_lossy().into_owned(),
    )];
    all_mounts.extend(mounts);
    AgentOs::create(AgentOsConfig {
        loopback_exempt_ports,
        mounts: all_mounts,
        permissions,
        ..Default::default()
    })
    .await
    .expect("create VM against real sidecar")
}

fn wasm_commands_dir() -> Option<PathBuf> {
    coreutils_wasm_dir()
}

fn wasm_command_mounts() -> Vec<MountConfig> {
    let Some(host_path) = wasm_commands_dir() else {
        return Vec::new();
    };

    vec![MountConfig::Native {
        path: "/__agentos/commands/0".to_string(),
        plugin: MountPlugin {
            id: "host_dir".to_string(),
            config: Some(serde_json::json!({
                "hostPath": host_path.to_string_lossy().into_owned(),
                "readOnly": true,
            })),
        },
        guest_source: Some("host_dir".to_string()),
        guest_fstype: Some("host_dir".to_string()),
        read_only: true,
    }]
}

/// Locate the materialized coreutils package under the in-repo registry build.
pub fn coreutils_package_dir() -> Option<PathBuf> {
    let registry_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../software/coreutils/dist/package");
    if registry_dir.join("agentos-package.json").is_file() {
        return std::fs::canonicalize(registry_dir).ok();
    }

    let node_modules_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../node_modules/@agentos-software/coreutils/dist/package");
    if node_modules_dir.join("agentos-package.json").is_file() {
        return std::fs::canonicalize(node_modules_dir).ok();
    }

    None
}

/// Locate the coreutils wasm command directory. Returns its canonical absolute path, or `None` when
/// the artifacts have not been installed/built.
pub fn coreutils_wasm_dir() -> Option<PathBuf> {
    if let Some(package_dir) = coreutils_package_dir() {
        let registry_bin = package_dir.join("bin");
        if registry_bin.is_dir() {
            return std::fs::canonicalize(registry_bin).ok();
        }
    }

    let legacy_registry_dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../software/coreutils/wasm");
    if legacy_registry_dir.is_dir() {
        return std::fs::canonicalize(legacy_registry_dir).ok();
    }

    let pnpm = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../node_modules/.pnpm");
    for entry in std::fs::read_dir(&pnpm).ok()?.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();

        if file_name.starts_with("@agentos-software+coreutils@") {
            let wasm = entry
                .path()
                .join("node_modules/@agentos-software/coreutils/wasm");
            if wasm.is_dir() {
                return std::fs::canonicalize(&wasm).ok();
            }
        }

        if file_name.starts_with("@rivet-dev+agentos-coreutils@") {
            let wasm = entry
                .path()
                .join("node_modules/@agentos-software/coreutils/wasm");
            if wasm.is_dir() {
                return std::fs::canonicalize(&wasm).ok();
            }
        }
    }
    None
}

/// Create a VM with the coreutils command package projected, so `exec`/`spawn` can resolve real
/// commands (`echo`, `cat`, `sh`, ...). Returns `None` when the package artifacts are absent, so
/// suites can skip cleanly in unbuilt trees.
pub async fn new_vm_with_commands() -> Option<AgentOs> {
    ensure_sidecar_env();
    let package_dir = coreutils_package_dir()?;
    let config = AgentOsConfig {
        packages: vec![PackageRef {
            path: package_dir.to_string_lossy().into_owned(),
        }],
        ..Default::default()
    };
    Some(
        AgentOs::create(config)
            .await
            .expect("create VM with coreutils command package"),
    )
}

/// Probe whether WASM-backed commands resolve in the VM (a trivial `exec`). Returns false when the
/// registry WASM command packages are absent (the common case in unbuilt trees), so the
/// process/shell/fetch suites can gate cleanly without each re-implementing the probe.
pub async fn wasm_commands_available(os: &AgentOs) -> bool {
    os.exec_process("sh", agentos_client::ExecOptions::default())
        .await
        .is_ok()
}

pub async fn require_wasm_commands(os: &AgentOs, test_name: &str) -> bool {
    if wasm_commands_available(os).await {
        return true;
    }

    let message = format!("{test_name}: WASM command packages are not available in the VM");
    if allow_local_e2e_skips() {
        eprintln!("skipping {message}");
        false
    } else {
        panic!("{message}; run the toolchain command build or set AGENT_OS_CLIENT_ALLOW_E2E_SKIPS=1 for local skip-only runs");
    }
}
