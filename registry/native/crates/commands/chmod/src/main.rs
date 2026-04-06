#![cfg_attr(target_os = "wasi", feature(wasi_ext))]

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        pub fn chmod(path_ptr: *const u8, path_len: u32, mode: u32) -> u32;
    }
}

#[cfg(not(target_os = "wasi"))]
use std::os::unix::fs::PermissionsExt;

fn main() {
    std::process::exit(run(std::env::args().skip(1)));
}

fn run(args: impl IntoIterator<Item = String>) -> i32 {
    let mut args = args.into_iter();
    let Some(mode_arg) = args.next() else {
        eprintln!("chmod: missing mode");
        return 1;
    };

    let mode = match u32::from_str_radix(mode_arg.trim_start_matches('0'), 8) {
        Ok(mode) => mode,
        Err(_) => {
            eprintln!("chmod: invalid mode '{mode_arg}'");
            return 1;
        }
    };

    let mut applied = false;
    for path in args {
        applied = true;
        if let Err(error) = apply_mode(&path, mode) {
            eprintln!("chmod: {path}: {error}");
            return 1;
        }
    }

    if !applied {
        eprintln!("chmod: missing operand");
        return 1;
    }

    0
}

#[cfg(target_os = "wasi")]
fn apply_mode(path: &str, mode: u32) -> std::io::Result<()> {
    let errno = unsafe { host_fs::chmod(path.as_ptr(), path.len() as u32, mode & 0o7777) };
    if errno == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(errno as i32))
    }
}

#[cfg(not(target_os = "wasi"))]
fn apply_mode(path: &str, mode: u32) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
}
