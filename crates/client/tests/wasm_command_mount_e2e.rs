//! Regression repro for "projected command packages resolve commands in the VM".
//!
//! A command package (e.g. `@agentos-software/coreutils`) must be projected into `/opt/agentos`
//! so the sidecar's command discovery can resolve guest commands. Before package projection was the
//! sole boot path, stale helpers could create a VM with no usable command package and
//! `exec("echo hello")` failed with `command not found on native sidecar path: echo hello`.
//!
//! This suite self-gates: it skips (returns early) when the sidecar binary is not built or when the
//! coreutils package artifacts are absent, so it stays honest in unbuilt trees. When both prerequisites
//! are present it asserts the real contract: `echo hello` exits 0 with stdout `hello`.

mod common;

use agentos_client::ExecOptions;

#[tokio::test]
async fn wasm_command_software_mounts_into_vm() {
    if !common::sidecar_available() {
        eprintln!("skipping wasm_command_software_mounts_into_vm: sidecar binary not built");
        return;
    }
    let Some(os) = common::new_vm_with_commands().await else {
        eprintln!(
            "skipping wasm_command_software_mounts_into_vm: coreutils package artifacts absent"
        );
        return;
    };

    // The TODO's exact verification: a mounted wasm command runs via `exec` and returns its output.
    // Before the mount fix this failed with "command not found"; before the exec command-line fix
    // the space made the whole string resolve as one command name.
    let result = os
        .exec_process("echo hello", ExecOptions::default())
        .await
        .expect("exec echo hello");
    assert_eq!(
        result.exit_code, 0,
        "echo should exit 0 (stderr: {:?})",
        result.stderr
    );
    assert_eq!(result.stdout.trim_end(), "hello", "echo stdout");

    os.shutdown().await.expect("shutdown");
}
