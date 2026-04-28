//! Shim implementation of the `stdbuf` command.
//!
//! In WASI, buffering modes cannot be set externally, so stdbuf
//! just runs the command directly.
//!
//! Usage: stdbuf [-i MODE] [-o MODE] [-e MODE] COMMAND [ARG]...

use std::ffi::OsString;
use std::process::Stdio;

pub fn stdbuf(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    // Skip buffering options, find the command
    let mut cmd_start = 0;
    let mut i = 0;
    while i < str_args.len() {
        let arg = &str_args[i];
        if (arg == "-i"
            || arg == "-o"
            || arg == "-e"
            || arg == "--input"
            || arg == "--output"
            || arg == "--error")
            && i + 1 < str_args.len()
        {
            i += 2;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            cmd_start = i;
            break;
        }
    }

    if cmd_start >= str_args.len() {
        eprintln!("stdbuf: missing operand");
        return 125;
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
            eprintln!("stdbuf: failed to run command '{}': {}", program, e);
            127
        }
    }
}
