//! Shim implementation of the `nohup` command.
//!
//! In WASI, there are no signals, so nohup just runs the command directly.
//!
//! Usage: nohup COMMAND [ARG]...

use std::ffi::OsString;
use std::process::Stdio;

pub fn nohup(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    if str_args.is_empty() {
        eprintln!("nohup: missing operand");
        return 127;
    }

    let program = &str_args[0];
    let child_args = &str_args[1..];

    let mut cmd = std::process::Command::new(program);
    cmd.args(child_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("nohup: failed to run command '{}': {}", program, e);
            127
        }
    }
}
