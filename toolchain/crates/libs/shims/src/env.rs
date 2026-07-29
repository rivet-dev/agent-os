//! Shim implementation of the `env` command.
//!
//! Sets environment variables and spawns a child process via
//! std::process::Command (which delegates to wasi-ext proc_spawn).
//!
//! Usage:
//!   env [OPTION]... [VAR=VALUE]... [COMMAND [ARG]...]
//!
//! Options:
//!   -i, --ignore-environment  Start with an empty environment
//!   -u, --unset VAR           Remove VAR from the environment

use std::ffi::OsString;
use std::io::{self, Write};

pub fn env(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0] ("env")
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut ignore_env = false;
    let mut unset_vars: Vec<String> = Vec::new();
    let mut set_vars: Vec<(String, String)> = Vec::new();
    let mut cmd_start = None;

    let mut i = 0;
    while i < str_args.len() {
        let arg = &str_args[i];
        if arg == "-i" || arg == "--ignore-environment" {
            ignore_env = true;
            i += 1;
        } else if arg == "-u" || arg == "--unset" {
            i += 1;
            if i < str_args.len() {
                unset_vars.push(str_args[i].clone());
                i += 1;
            } else {
                eprintln!("env: option requires an argument -- 'u'");
                return 125;
            }
        } else if let Some(eq_pos) = arg.find('=') {
            set_vars.push((arg[..eq_pos].to_string(), arg[eq_pos + 1..].to_string()));
            i += 1;
        } else {
            // First non-option, non-VAR=VALUE argument is the command
            cmd_start = Some(i);
            break;
        }
    }

    if cmd_start.is_none() {
        if let Err(e) = print_env(ignore_env, &unset_vars, &set_vars) {
            eprintln!("env: {}", e);
            return 1;
        }
        return 0;
    }

    // Build child command
    let cmd_idx = cmd_start.unwrap();
    let program = &str_args[cmd_idx];
    let child_args = &str_args[cmd_idx + 1..];

    let mut cmd = std::process::Command::new(crate::which::resolve_program(program));
    cmd.args(child_args);

    if ignore_env {
        cmd.env_clear();
    }
    for var in &unset_vars {
        cmd.env_remove(var);
    }
    for (key, value) in &set_vars {
        cmd.env(key, value);
    }

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("env: '{}': {}", program, e);
            127
        }
    }
}

fn print_env(
    ignore_env: bool,
    unset_vars: &[String],
    set_vars: &[(String, String)],
) -> io::Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if ignore_env {
        // Only print explicitly set vars.
        for (key, value) in set_vars {
            writeln!(out, "{}={}", key, value)?;
        }
    } else {
        // Print inherited env (minus unset vars) plus set vars.
        for (key, value) in std::env::vars() {
            if !unset_vars.contains(&key) {
                writeln!(out, "{}={}", key, value)?;
            }
        }
        for (key, value) in set_vars {
            writeln!(out, "{}={}", key, value)?;
        }
    }

    out.flush()
}
