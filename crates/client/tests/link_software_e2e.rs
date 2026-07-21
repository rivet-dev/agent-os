//! End-to-end coverage for `link_software` (Rust-client parity with the TS
//! `linkSoftware`): a package added to a running VM resolves its `bin/` command
//! live via `$PATH`.

use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use agentos_client::config::{
    AgentOsConfig, FsPermissions, PatternPermissions, PermissionMode, Permissions,
};
use agentos_client::process::SpawnOptions;
use agentos_client::AgentOs;
use agentos_client::PackageDescriptor;

fn allow_all() -> Permissions {
    Permissions {
        fs: Some(FsPermissions::Mode(PermissionMode::Allow)),
        network: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        child_process: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        process: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        env: Some(PatternPermissions::Mode(PermissionMode::Allow)),
        binding: Some(PatternPermissions::Mode(PermissionMode::Allow)),
    }
}

mod common;

#[tokio::test]
async fn link_software_makes_command_resolve_live() {
    if !common::require_sidecar("link_software_makes_command_resolve_live") {
        return;
    }

    // Build a self-contained, dependency-free package on the host.
    let dir = std::env::temp_dir().join(format!("agentos-link-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("bin")).expect("mkdir bin");
    std::fs::write(
        dir.join("agentos-package.json"),
        r#"{"name":"linked-tool","version":"1.0.0"}"#,
    )
    .expect("write agentos-package.json");
    let bin = dir.join("bin").join("linked-cmd");
    std::fs::write(
        &bin,
        "#!/usr/bin/env node\nprocess.stdout.write('linked-rust-ok\\n');\n",
    )
    .expect("write bin");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let os = AgentOs::create(AgentOsConfig {
        permissions: Some(allow_all()),
        ..AgentOsConfig::default()
    })
    .await
    .expect("create VM");

    os.link_software(PackageDescriptor {
        path: dir.to_string_lossy().into_owned(),
    })
    .await
    .expect("link_software");

    // The /opt/agentos mount is host-backed, so the linked command must be visible.
    let exists = os
        .exists("/opt/agentos/bin/linked-cmd")
        .await
        .expect("exists check");
    assert!(
        exists,
        "/opt/agentos/bin/linked-cmd should exist after link_software"
    );

    // Spawn directly (no shell needed) so the test isolates the linked command's
    // $PATH resolution + header dispatch.
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err_cap = Arc::new(Mutex::new(Vec::<u8>::new()));
    let handle = os
        .spawn_process("linked-cmd", Vec::new(), SpawnOptions::default())
        .expect("spawn linked-cmd");
    let cb = captured.clone();
    let ecb = err_cap.clone();
    let _output = os
        .on_process_output(handle.pid, move |event| match event.stream {
            agentos_client::ProcessStream::Stdout => {
                cb.lock().unwrap().extend_from_slice(&event.data)
            }
            agentos_client::ProcessStream::Stderr => {
                ecb.lock().unwrap().extend_from_slice(&event.data)
            }
        })
        .expect("subscribe linked-cmd output");
    let code = os.wait_process(handle.pid).await.expect("wait linked-cmd");
    let stdout = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&err_cap.lock().unwrap()).into_owned();
    assert_eq!(code, 0, "exit code; stdout={stdout:?} stderr={stderr:?}");
    assert!(stdout.contains("linked-rust-ok"), "stdout: {stdout:?}");

    os.shutdown().await.ok();
    let _ = std::fs::remove_dir_all(&dir);
}
