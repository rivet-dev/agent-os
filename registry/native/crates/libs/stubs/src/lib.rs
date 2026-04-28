//! Stub implementations for commands that are impossible or impractical in WASM.
//!
//! Each stub prints an appropriate error message and exits with code 1,
//! except for commands that can provide a reasonable default (hostname,
//! hostid, sync).
//!
//! Used by the _stubs mini-multicall binary, which dispatches on argv[0].

use std::path::Path;

/// Run a stub command based on argv[0] basename.
/// Returns exit code (0 for no-ops/defaults, 1 for unsupported).
pub fn run(args: &[String]) -> i32 {
    let cmd = args
        .first()
        .and_then(|a| Path::new(a).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("_stubs");

    match cmd {
        "chcon" | "runcon" => {
            eprintln!("{}: SELinux is not supported in WASM", cmd);
            1
        }
        "chgrp" | "chown" => {
            eprintln!(
                "{}: user/group ownership changes are not supported in WASM",
                cmd
            );
            1
        }
        "chroot" => {
            eprintln!("chroot: not supported in WASM (no filesystem root change)");
            1
        }
        "df" => {
            eprintln!("df: filesystem stats are not available in WASM");
            1
        }
        "groups" | "id" => {
            eprintln!("{}: user database queries are not supported in WASM", cmd);
            1
        }
        "hostname" => {
            println!("wasm-host");
            0
        }
        "hostid" => {
            println!("00000000");
            0
        }
        "install" => {
            eprintln!("install: file permission management not fully supported in WASM");
            1
        }
        "kill" => {
            eprintln!("kill: process signals are not supported in WASM");
            1
        }
        "mkfifo" | "mknod" => {
            eprintln!("{}: special file creation is not supported in WASM", cmd);
            1
        }
        "pinky" | "who" | "users" | "uptime" => {
            eprintln!("{}: login records (utmp) are not available in WASM", cmd);
            1
        }
        "stty" => {
            eprintln!("stty: terminal control is not supported in WASM");
            1
        }
        "sync" => {
            // No-op in WASM (VFS is in-memory)
            0
        }
        "tty" => {
            eprintln!("not a tty");
            1
        }
        _ => {
            eprintln!("{}: command not supported in sandbox", cmd);
            1
        }
    }
}
