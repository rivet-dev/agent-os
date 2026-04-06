#![cfg_attr(target_os = "wasi", feature(wasi_ext))]

#[cfg(target_os = "wasi")]
use std::os::wasi::fs::symlink as symlink_file;
#[cfg(unix)]
use std::os::unix::fs::symlink as symlink_file;
use std::path::Path;

fn main() {
    std::process::exit(run(std::env::args_os().skip(1)));
}

fn run(args: impl IntoIterator<Item = std::ffi::OsString>) -> i32 {
    let mut force = false;
    let mut symbolic = false;
    let mut paths = Vec::new();

    for arg in args {
        let arg = arg.to_string_lossy();
        if arg == "--" {
            continue;
        }
        if paths.is_empty() && arg.starts_with('-') && arg != "-" {
            for flag in arg.chars().skip(1) {
                match flag {
                    's' => symbolic = true,
                    'f' | 'n' | 'T' => force = true,
                    other => {
                        eprintln!("ln: unsupported option -- '{other}'");
                        return 1;
                    }
                }
            }
            continue;
        }
        paths.push(arg.to_string());
    }

    if paths.len() != 2 {
        eprintln!("ln: expected SOURCE and DEST");
        return 1;
    }

    let target = &paths[0];
    let link_path = &paths[1];

    if force {
        match std::fs::symlink_metadata(link_path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                eprintln!("ln: cannot overwrite directory '{}'", link_path);
                return 1;
            }
            Ok(_) => {
                if let Err(error) = std::fs::remove_file(link_path) {
                    eprintln!("ln: {}", error);
                    return 1;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("ln: {}", error);
                return 1;
            }
        }
    }

    let result = if symbolic {
        match create_symlink(target, link_path) {
            Ok(()) => Ok(()),
            Err(error) if should_fallback_to_hard_link(&error) => std::fs::hard_link(target, link_path),
            Err(error) => Err(error),
        }
    } else {
        std::fs::hard_link(target, link_path)
    };

    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ln: {}", error);
            1
        }
    }
}

#[cfg(target_os = "wasi")]
fn create_symlink(target: &str, link_path: &str) -> std::io::Result<()> {
    let link_path = Path::new(link_path);
    let parent = link_path.parent().unwrap_or_else(|| Path::new("."));
    let name = link_path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "missing link name")
    })?;
    let dir = std::fs::File::open(parent)?;
    symlink_file(target, &dir, name)
}

#[cfg(not(target_os = "wasi"))]
fn create_symlink(target: &str, link_path: &str) -> std::io::Result<()> {
    symlink_file(target, link_path)
}

fn should_fallback_to_hard_link(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
    ) || matches!(error.raw_os_error(), Some(63))
}
