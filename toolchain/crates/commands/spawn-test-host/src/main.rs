/// Test binary for WasiChild: exercises host_process spawn with pipe capture.
///
/// Subcommands:
///   echo       — spawn "echo hello" and print captured stdout
///   tokio-bash — run the exact shell/stdio/cwd shape used by Codex
///   fail       — spawn a command that exits non-zero and print exit code
///   kill-test  — spawn "sleep 60", kill it, verify termination
///   env-test   — spawn "env" with custom env vars and print captured stdout

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let subcommand = args.get(1).map(|s| s.as_str()).unwrap_or("echo");

    let code = match subcommand {
        "echo" => test_echo(),
        "tokio-bash" => test_tokio_bash(),
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

fn test_tokio_bash() -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
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
