#![cfg_attr(target_os = "wasi", feature(wasi_ext))]

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        pub fn path_mode(path_ptr: *const u8, path_len: u32, follow_symlinks: u32) -> u32;
    }
}

#[cfg(not(target_os = "wasi"))]
use std::os::unix::fs::PermissionsExt;

fn main() {
    std::process::exit(run(std::env::args().skip(1)));
}

fn run(args: impl IntoIterator<Item = String>) -> i32 {
    let mut args = args.into_iter();
    let Some(first) = args.next() else {
        eprintln!("stat: missing arguments");
        return 1;
    };
    let format = if first == "-c" {
        match args.next() {
            Some(format) => format,
            None => {
                eprintln!("stat: missing format");
                return 1;
            }
        }
    } else if let Some(rest) = first.strip_prefix("--format=") {
        rest.to_string()
    } else {
        eprintln!("stat: unsupported arguments");
        return 1;
    };
    let Some(path) = args.next() else {
        eprintln!("stat: missing operand");
        return 1;
    };
    if args.next().is_some() {
        eprintln!("stat: too many operands");
        return 1;
    }

    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("stat: {path}: {error}");
            return 1;
        }
    };
    let mode = mode_for_path(&path);
    let rendered = render_format(&format, metadata.is_dir(), mode);
    println!("{rendered}");
    0
}

#[cfg(target_os = "wasi")]
fn mode_for_path(path: &str) -> u32 {
    unsafe { host_fs::path_mode(path.as_ptr(), path.len() as u32, 1) }
}

#[cfg(not(target_os = "wasi"))]
fn mode_for_path(path: &str) -> u32 {
    std::fs::metadata(path)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0)
}

fn render_format(format: &str, is_dir: bool, mode: u32) -> String {
    format
        .replace("%a", &format!("{:o}", mode & 0o7777))
        .replace("%A", &render_mode_string(is_dir, mode))
}

fn render_mode_string(is_dir: bool, mode: u32) -> String {
    let mut rendered = String::with_capacity(10);
    rendered.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        rendered.push(if mode & (0o4 << shift) != 0 { 'r' } else { '-' });
        rendered.push(if mode & (0o2 << shift) != 0 { 'w' } else { '-' });
        rendered.push(if mode & (0o1 << shift) != 0 { 'x' } else { '-' });
    }
    rendered
}
