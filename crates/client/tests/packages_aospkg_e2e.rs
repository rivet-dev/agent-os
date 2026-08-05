//! End-to-end boot of a VM from a packed `.aospkg` registry package — the
//! production path: `packages: [{ path: <coreutils .aospkg> }]` → sidecar
//! `/opt/agentos` projection (tar-vfs leaf mounts from the packed index) →
//! wasm commands resolve on `$PATH` and execute. Skips cleanly when the
//! coreutils package has not been built (`pnpm build`).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use agentos_client::config::{AgentOsConfig, PackageRef};
use agentos_client::process::SpawnOptions;
use agentos_client::AgentOs;

mod common;

fn coreutils_aospkg() -> Option<PathBuf> {
    for path in [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../software/coreutils/dist/package.aospkg"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../node_modules/@agentos-software/coreutils/dist/package.aospkg"),
    ] {
        if path.is_file() {
            return std::fs::canonicalize(path).ok();
        }
    }
    None
}

async fn spawn_capture(os: &AgentOs, cmd: &str, args: Vec<String>) -> (i32, String, String) {
    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
    let err_cap = Arc::new(Mutex::new(Vec::<u8>::new()));
    let handle = os
        .spawn(cmd, args, SpawnOptions::default())
        .unwrap_or_else(|e| panic!("spawn {cmd}: {e:?}"));
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
        .unwrap_or_else(|e| panic!("subscribe {cmd}: {e:?}"));
    let code = os
        .wait_process(handle.pid)
        .await
        .unwrap_or_else(|e| panic!("wait {cmd}: {e:?}"));
    let stdout = String::from_utf8_lossy(&captured.lock().unwrap()).into_owned();
    let stderr = String::from_utf8_lossy(&err_cap.lock().unwrap()).into_owned();
    (code, stdout, stderr)
}

#[tokio::test(flavor = "multi_thread")]
async fn vm_boots_and_runs_coreutils_from_packed_aospkg() {
    if !common::require_sidecar("vm_boots_and_runs_coreutils_from_packed_aospkg") {
        return;
    }
    let Some(aospkg) = coreutils_aospkg() else {
        eprintln!("skipping: coreutils package.aospkg is not built (run `pnpm build`)");
        return;
    };

    common::ensure_sidecar_env();
    let os = AgentOs::create(AgentOsConfig {
        packages: vec![PackageRef {
            path: aospkg.to_string_lossy().into_owned(),
        }],
        ..Default::default()
    })
    .await
    .expect("create VM with packed coreutils");

    // The projection links coreutils commands onto $PATH.
    assert!(
        os.exists("/opt/agentos/bin/ls").await.expect("exists"),
        "/opt/agentos/bin/ls must be projected from the packed package"
    );
    // The packed mount tar must NOT expose the pack-time manifest input.
    assert!(
        !os.exists("/opt/agentos/pkgs/coreutils/current/agentos-package.json")
            .await
            .expect("exists"),
        "agentos-package.json is toolchain input and must not ship in the mount tar"
    );

    // Run real coreutils commands out of the packed tar mount.
    let (code, stdout, stderr) = spawn_capture(&os, "ls", vec![String::from("/")]).await;
    assert_eq!(code, 0, "ls / exit; stdout={stdout:?} stderr={stderr:?}");
    assert!(
        stdout.contains("opt"),
        "ls / must list /opt: stdout={stdout:?} stderr={stderr:?}"
    );

    let (code, stdout, stderr) =
        spawn_capture(&os, "cat", vec![String::from("/etc/hostname")]).await;
    assert_eq!(
        code, 0,
        "cat /etc/hostname exit; stdout={stdout:?} stderr={stderr:?}"
    );

    let (code, stdout, _stderr) =
        spawn_capture(&os, "ls", vec![String::from("/opt/agentos/bin")]).await;
    assert_eq!(code, 0, "ls /opt/agentos/bin exit");
    assert!(
        stdout.contains("ls") && stdout.contains("cat"),
        "projected bin listing: {stdout:?}"
    );

    os.shutdown().await.ok();
}
