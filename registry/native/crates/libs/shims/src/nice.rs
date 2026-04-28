//! Shim implementation of the `nice` command.
//!
//! In WASI, process priority has no effect, so we just pass through
//! to spawn the child command.
//!
//! Usage: nice [-n ADJUSTMENT] COMMAND [ARG]...

use std::ffi::OsString;
use std::process::Stdio;

pub fn nice(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut cmd_start = 0;

    // Skip -n ADJUSTMENT if present
    if str_args.len() >= 2 && str_args[0] == "-n" {
        cmd_start = 2;
    } else if !str_args.is_empty() && str_args[0].starts_with('-') {
        // Handle -N form
        if str_args[0][1..].parse::<i32>().is_ok() {
            cmd_start = 1;
        }
    }

    if cmd_start >= str_args.len() {
        // No command — just print 0 (the nice value)
        println!("0");
        return 0;
    }

    let program = &str_args[cmd_start];
    let child_args = &str_args[cmd_start + 1..];

    let mut cmd = std::process::Command::new(program);
    cmd.args(child_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("nice: '{}': {}", program, e);
            127
        }
    }
}
