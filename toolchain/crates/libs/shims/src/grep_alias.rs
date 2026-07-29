//! GNU grep obsolescent alias launchers.
//!
//! Upstream GNU grep installs `egrep` and `fgrep` as scripts that warn and exec
//! `grep -E` / `grep -F`. Registry commands are WASM modules, so these compiled
//! launchers preserve the upstream script behavior through the process broker.

use std::ffi::OsString;
use std::process::Stdio;

pub fn egrep(args: Vec<OsString>) -> i32 {
    run_alias(args, "egrep", "-E")
}

pub fn fgrep(args: Vec<OsString>) -> i32 {
    run_alias(args, "fgrep", "-F")
}

fn run_alias(args: Vec<OsString>, name: &str, option: &str) -> i32 {
    eprintln!("{name}: warning: {name} is obsolescent; using grep {option}");

    let mut command = std::process::Command::new(crate::which::resolve_program("grep"));
    command
        .arg(option)
        .args(args.into_iter().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("{name}: failed to run grep: {error}");
            127
        }
    }
}
