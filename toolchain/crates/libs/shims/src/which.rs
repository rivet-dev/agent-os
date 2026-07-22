//! Minimal `which` implementation for the agentos VM.
//!
//! Searches the current PATH for one or more command names and prints the first
//! matching executable path for each command. This is primarily needed for
//! agent CLIs such as Claude Code, which probe for available shells with
//! commands like `which zsh` / `which bash`.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        // Signature must match the sidecar host_fs.path_mode
        // (dir_fd, path_ptr, path_len, follow_symlinks).
        pub fn path_mode(
            dir_fd: u32,
            path_ptr: *const u8,
            path_len: u32,
            follow_symlinks: u32,
        ) -> u32;
    }
}

#[cfg(target_os = "wasi")]
const AGENTOS_CWD_FD: u32 = u32::MAX;

fn print_usage<W: Write>(out: &mut W) -> io::Result<()> {
    writeln!(out, "Usage: which [-a] name [...]")
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
    let Ok(path_len) = u32::try_from(bytes.len()) else {
        return false;
    };
    // The host extension uses this sentinel for a pathname resolved from the
    // process cwd. Private WASI preopens do not occupy guest-visible fd 3.
    let mode = unsafe {
        host_fs::path_mode(AGENTOS_CWD_FD, bytes.as_ptr(), path_len, 1)
    };
    (mode & 0o111) != 0
}

#[cfg(not(any(unix, target_os = "wasi")))]
fn executable_mode_bits(_path: &Path, metadata: &fs::Metadata) -> bool {
    !metadata.permissions().readonly()
}

fn search_path<F>(command: &str, all: bool, mut on_match: F) -> io::Result<bool>
where
    F: FnMut(&Path) -> io::Result<()>,
{
    if command.contains('/') {
        let path = PathBuf::from(command);
        if is_executable_path(&path) {
            on_match(&path)?;
            return Ok(true);
        }
        return Ok(false);
    }

    let mut found = false;
    let path_var = std::env::var("PATH").unwrap_or_default();
    for dir in path_var.split(':').filter(|segment| !segment.is_empty()) {
        let candidate = Path::new(dir).join(command);
        if is_executable_path(&candidate) {
            on_match(&candidate)?;
            found = true;
            if !all {
                break;
            }
        }
    }

    Ok(found)
}

pub(crate) fn resolve_program(command: &str) -> PathBuf {
    let mut resolved = None;
    let _ = search_path(command, false, |path| {
        resolved = Some(path.to_path_buf());
        Ok(())
    });
    resolved.unwrap_or_else(|| PathBuf::from(command))
}

pub fn which(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut all = false;
    let mut commands = Vec::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for arg in str_args {
        match arg.as_str() {
            "-a" => all = true,
            "--help" => {
                return match print_usage(&mut out).and_then(|_| out.flush()) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("which: {}", e);
                        2
                    }
                };
            }
            "--version" => {
                return match writeln!(out, "which 0.1.0").and_then(|_| out.flush()) {
                    Ok(()) => 0,
                    Err(e) => {
                        eprintln!("which: {}", e);
                        2
                    }
                };
            }
            _ if arg.starts_with('-') => {
                eprintln!("which: unsupported option '{}'", arg);
                return 2;
            }
            _ => commands.push(arg),
        }
    }

    if commands.is_empty() {
        return match print_usage(&mut out).and_then(|_| out.flush()) {
            Ok(()) => 2,
            Err(e) => {
                eprintln!("which: {}", e);
                2
            }
        };
    }

    let mut found_all = true;

    for command in commands {
        match search_path(&command, all, |path| writeln!(out, "{}", path.display())) {
            Ok(true) => {}
            Ok(false) => found_all = false,
            Err(e) => {
                eprintln!("which: {}", e);
                return 2;
            }
        }
    }

    if let Err(e) = out.flush() {
        eprintln!("which: {}", e);
        return 2;
    }

    if found_all {
        0
    } else {
        1
    }
}
