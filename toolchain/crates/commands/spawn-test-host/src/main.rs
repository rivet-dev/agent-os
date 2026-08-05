/// Test binary for WasiChild: exercises host_process spawn with pipe capture.
///
/// Subcommands:
///   echo       — spawn "echo hello" and print captured stdout
///   tokio-bash — run the exact shell/stdio/cwd shape used by Codex
///   tokio-large-output — drain a captured Tokio child pipe past capacity
///   emit-large-output — internal child used by tokio-large-output
///   fail       — spawn a command that exits non-zero and print exit code
///   kill-test  — spawn "sleep 60", kill it, verify termination
///   env-test   — spawn "env" with custom env vars and print captured stdout

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("echo");

    let code = match subcommand {
        "echo" => test_echo(),
        "tokio-bash" => test_tokio_bash(),
        "tokio-large-output" => test_tokio_large_output(),
        "emit-large-output" => emit_large_output(),
        "fail" => test_fail(),
        "kill-test" => test_kill(),
        "env-test" => test_env(),
        _ => {
            eprintln!("spawn-test-host: unknown subcommand '{}'", subcommand);
            1
        }
    };

    std::process::exit(code);
}

fn tokio_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|error| format!("tokio runtime: {error}"))
}

fn test_tokio_bash() -> i32 {
    let runtime = match tokio_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("tokio-bash:runtime-error:{error}");
            return 1;
        }
    };
    runtime.block_on(async {
        let mut command = tokio::process::Command::new("/opt/agentos/bin/bash");
        command
            .args(["-lc", "printf agentos-codex-shell-ok"])
            .current_dir("/workspace")
            .env_clear()
            .env("PATH", "/opt/agentos/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        match command.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if output.status.success() && stdout == "agentos-codex-shell-ok" {
                    println!("PASS");
                    0
                } else {
                    eprintln!("tokio-bash:unexpected-output:{}:{stdout}", output.status);
                    1
                }
            }
            Err(error) => {
                eprintln!("tokio-bash:output-error:{error}");
                1
            }
        }
    })
}

fn test_tokio_large_output() -> i32 {
    const OUTPUT_ROWS: usize = 6_000;
    let runtime = match tokio_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("tokio-large-output:runtime-error:{error}");
            return 1;
        }
    };
    runtime.block_on(async {
        // Spawn this already-loaded helper so the regression measures Tokio
        // pipe backpressure rather than Wasmtime cold compilation of unrelated
        // shell and awk modules. Each emitted row is at least 11 bytes, keeping
        // the captured stream above the 64 KiB kernel pipe capacity.
        let mut command = tokio::process::Command::new("/opt/agentos/bin/spawn-test-host");
        command
            .arg("emit-large-output")
            .current_dir("/workspace")
            .env_clear()
            .env("PATH", "/opt/agentos/bin")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        match command.output().await {
            Ok(output) => {
                let rows = output.stdout.iter().filter(|byte| **byte == b'\n').count();
                if output.status.success()
                    && output.stdout.len() > 65_536
                    && rows == OUTPUT_ROWS
                    && output.stderr.is_empty()
                {
                    println!("PASS bytes={} rows={rows}", output.stdout.len());
                    0
                } else {
                    eprintln!(
                        "tokio-large-output:unexpected-output:{}:bytes={}:rows={rows}:stderr={}",
                        output.status,
                        output.stdout.len(),
                        String::from_utf8_lossy(&output.stderr)
                    );
                    1
                }
            }
            Err(error) => {
                eprintln!("tokio-large-output:output-error:{error}");
                1
            }
        }
    })
}

fn emit_large_output() -> i32 {
    use std::io::Write as _;

    let stdout = std::io::stdout();
    let mut stdout = std::io::BufWriter::new(stdout.lock());
    for row in 1..=6_000 {
        if let Err(error) = writeln!(stdout, "captured:{row}") {
            eprintln!("emit-large-output:write-error:{error}");
            return 1;
        }
    }
    match stdout.flush() {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("emit-large-output:flush-error:{error}");
            1
        }
    }
}

/// Test 1: spawn "echo hello", capture stdout, verify content
fn test_echo() -> i32 {
    let mut child = match wasi_spawn::spawn_child(&["/opt/agentos/bin/echo", "hello"], &[], "/") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL spawn: {}", e);
            return 1;
        }
    };

    match child.consume_output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            print!("stdout:{}", stdout);
            println!("exit:{}", output.exit_code);
            if stdout.trim() == "hello" && output.exit_code == 0 {
                println!("PASS");
                0
            } else {
                println!("FAIL");
                1
            }
        }
        Err(e) => {
            eprintln!("FAIL consume: {}", e);
            1
        }
    }
}

/// Test 2: spawn a command that exits non-zero, verify exit code
fn test_fail() -> i32 {
    let mut child =
        match wasi_spawn::spawn_child(&["/opt/agentos/bin/sh", "-c", "exit 42"], &[], "/") {
            Ok(c) => c,
            Err(e) => {
                eprintln!("FAIL spawn: {}", e);
                return 1;
            }
        };

    match child.consume_output() {
        Ok(output) => {
            println!("exit:{}", output.exit_code);
            if output.exit_code == 42 {
                println!("PASS");
                0
            } else {
                println!("FAIL expected 42 got {}", output.exit_code);
                1
            }
        }
        Err(e) => {
            eprintln!("FAIL consume: {}", e);
            1
        }
    }
}

/// Test 3: spawn sleep, kill it, verify termination
fn test_kill() -> i32 {
    let mut child = match wasi_spawn::spawn_child(&["/opt/agentos/bin/sleep", "60"], &[], "/") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL spawn: {}", e);
            return 1;
        }
    };

    // Kill the child with SIGTERM
    if let Err(e) = child.terminate() {
        eprintln!("FAIL kill: {}", e);
        return 1;
    }

    match child.wait() {
        Ok(status) => {
            println!("exit:{}", status);
            // 128 + 15 (SIGTERM) = 143
            if status >= 128 {
                println!("PASS");
                0
            } else {
                println!("FAIL expected signal exit, got {}", status);
                1
            }
        }
        Err(e) => {
            eprintln!("FAIL wait: {}", e);
            1
        }
    }
}

/// Test 4: spawn env with custom variables, verify they appear
fn test_env() -> i32 {
    let mut child = match wasi_spawn::spawn_child(
        &["/opt/agentos/bin/env"],
        &[("TEST_VAR", "hello_world"), ("FOO", "bar")],
        "/",
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL spawn: {}", e);
            return 1;
        }
    };

    match child.consume_output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let has_test = stdout.contains("TEST_VAR=hello_world");
            let has_foo = stdout.contains("FOO=bar");
            println!("exit:{}", output.exit_code);
            if has_test && has_foo {
                println!("PASS");
                0
            } else {
                print!("{}", stdout);
                println!(
                    "FAIL missing env vars (TEST_VAR={}, FOO={})",
                    has_test, has_foo
                );
                1
            }
        }
        Err(e) => {
            eprintln!("FAIL consume: {}", e);
            1
        }
    }
}
