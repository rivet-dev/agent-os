//! End-to-end coverage for `exec`'s command-line handling against the real sidecar.
//!
//! `exec` takes a command *line*, not a pre-split argv. The client mirrors the sidecar's
//! child_process shell decision: shell-free commands spawn directly (preserving the command's real
//! exit code), while shell syntax or a POSIX builtin head runs under `sh -c <line>`. These tests
//! prove both paths produce truthful results inside the VM.
//!
//! Self-gating: skips when the sidecar binary or coreutils wasm artifacts are absent.

mod common;

use agentos_client::ExecOptions;

#[tokio::test]
async fn exec_command_line_paths() {
    if !common::sidecar_available() {
        eprintln!("skipping exec_command_line_paths: sidecar binary not built");
        return;
    }
    let Some(os) = common::new_vm_with_commands().await else {
        eprintln!("skipping exec_command_line_paths: coreutils wasm artifacts absent");
        return;
    };

    // Direct path: a simple command with arguments runs and returns its output.
    let echo = os
        .exec_process("echo hello world", ExecOptions::default())
        .await
        .expect("exec echo");
    assert_eq!(echo.exit_code, 0, "echo exit (stderr: {:?})", echo.stderr);
    assert_eq!(echo.stdout.trim_end(), "hello world", "echo stdout");

    // Direct path preserves a real non-zero exit code (the `sh -c` wrapper can swallow it).
    let missing = os
        .exec_process("cat /no/such/file", ExecOptions::default())
        .await
        .expect("exec cat missing returns a result");
    assert_ne!(
        missing.exit_code, 0,
        "cat of a missing file must report a non-zero exit code"
    );

    // Shell path: a `&&` chain runs as a single `sh -c` execution.
    let chain = os
        .exec_process("echo a && echo b", ExecOptions::default())
        .await
        .expect("exec && chain");
    assert_eq!(
        chain.exit_code, 0,
        "chain exit (stderr: {:?})",
        chain.stderr
    );
    assert_eq!(chain.stdout, "a\nb\n", "chain stdout");

    // Shell path: a redirect writes a file the VM can read back.
    let redirect = os
        .exec_process(
            "echo redirected > /tmp/exec_redirect.txt",
            ExecOptions::default(),
        )
        .await
        .expect("exec redirect");
    assert_eq!(
        redirect.exit_code, 0,
        "redirect exit (stderr: {:?})",
        redirect.stderr
    );
    let body = os
        .read_file("/tmp/exec_redirect.txt")
        .await
        .expect("read redirected file");
    assert_eq!(
        String::from_utf8_lossy(&body).trim_end(),
        "redirected",
        "redirected file contents"
    );

    // Shell path: a quoted argument with a space stays one token through `sh -c`.
    let quoted = os
        .exec_process("echo 'a b'", ExecOptions::default())
        .await
        .expect("exec quoted");
    assert_eq!(quoted.stdout.trim_end(), "a b", "quoted stdout");

    // An empty command line is an explicit error, not a silent no-op.
    assert!(
        os.exec_process("   ", ExecOptions::default())
            .await
            .is_err(),
        "empty command line must error"
    );

    os.shutdown().await.expect("shutdown");
}
