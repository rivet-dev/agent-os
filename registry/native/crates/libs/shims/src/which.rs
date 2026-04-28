//! Minimal `which` implementation for the Agent OS WasmVM.
//!
//! Searches the current PATH for one or more command names and prints the first
//! matching executable path for each command. This is primarily needed for
//! agent CLIs such as Claude Code, which probe for available shells with
//! commands like `which zsh` / `which bash`.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        pub fn path_mode(path_ptr: *const u8, path_len: u32, follow_symlinks: u32) -> u32;
    }
}

fn print_usage() {
    println!("Usage: which [-a] name [...]");
}

fn is_executable_path(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };

    metadata.is_file() && executable_mode_bits(path, &metadata)
}

#[cfg(unix)]
fn executable_mode_bits(_path: &Path, metadata: &fs::Metadata) -> bool {
    (metadata.mode() & 0o111) != 0
}

#[cfg(target_os = "wasi")]
fn executable_mode_bits(path: &Path, _metadata: &fs::Metadata) -> bool {
    let path_string = path.to_string_lossy();
    let bytes = path_string.as_bytes();
    let mode = unsafe { host_fs::path_mode(bytes.as_ptr(), bytes.len() as u32, 1) };
    (mode & 0o111) != 0
}

#[cfg(not(any(unix, target_os = "wasi")))]
fn executable_mode_bits(_path: &Path, metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

fn search_path(command: &str, all: bool) -> Vec<PathBuf> {
    if command.contains('/') {
        let path = PathBuf::from(command);
        return if is_executable_path(&path) {
            vec![path]
        } else {
            Vec::new()
        };
    }

    let mut matches = Vec::new();
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':').filter(|segment| !segment.is_empty()) {
        let candidate = Path::new(dir).join(command);
        if is_executable_path(&candidate) {
            matches.push(candidate);
            if !all {
                break;
            }
        }
    }

    matches
}

pub fn which(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut all = false;
    let mut commands = Vec::new();

    for arg in str_args {
        match arg.as_str() {
            "-a" => all = true,
            "--help" => {
                print_usage();
                return 0;
            }
            "--version" => {
                println!("which 0.1.0");
                return 0;
            }
            _ if arg.starts_with('-') => {
                eprintln!("which: unsupported option '{}'", arg);
                return 2;
            }
            _ => commands.push(arg),
        }
    }

    if commands.is_empty() {
        print_usage();
        return 2;
    }

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut found_all = true;

    for command in commands {
        let matches = search_path(&command, all);
        if matches.is_empty() {
            found_all = false;
            continue;
        }

        for path in matches {
            let _ = writeln!(out, "{}", path.display());
        }
    }

    if found_all {
        0
    } else {
        1
    }
}
