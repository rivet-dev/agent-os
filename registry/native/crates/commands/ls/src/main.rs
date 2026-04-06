#![cfg_attr(target_os = "wasi", feature(wasi_ext))]

#[cfg(target_os = "wasi")]
mod host_fs {
    #[link(wasm_import_module = "host_fs")]
    unsafe extern "C" {
        pub fn path_mode(path_ptr: *const u8, path_len: u32, follow_symlinks: u32) -> u32;
    }
}

use std::path::Path;
#[cfg(not(target_os = "wasi"))]
use std::os::unix::fs::PermissionsExt;

fn main() {
    std::process::exit(run(std::env::args().skip(1)));
}

fn run(args: impl IntoIterator<Item = String>) -> i32 {
    let mut long = false;
    let mut show_all = false;
    let mut target: Option<String> = None;

    for arg in args {
        if arg == "--" {
            continue;
        }
        if arg.starts_with('-') && arg != "-" {
            long |= arg.contains('l');
            show_all |= arg.contains('a');
            continue;
        }
        if target.is_some() {
            eprintln!("ls: multiple targets are not supported");
            return 1;
        }
        target = Some(arg);
    }

    let target = target.unwrap_or_else(|| ".".to_string());
    if long {
        return render_long_entry(&target);
    }
    render_simple_listing(&target, show_all)
}

fn render_long_entry(target: &str) -> i32 {
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("ls: {target}: {error}");
            return 1;
        }
    };
    let mode = mode_for_path(&target);
    let kind = if metadata.file_type().is_dir() {
        'd'
    } else if metadata.file_type().is_symlink() {
        'l'
    } else {
        '-'
    };
    let permissions = render_mode_string(mode);
    println!(
        "{kind}{permissions} 1 somebody somegroup {} Jan  1 00:00 {}",
        metadata.len(),
        target
    );
    0
}

fn render_simple_listing(target: &str, show_all: bool) -> i32 {
    let path = Path::new(target);
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("ls: {target}: {error}");
            return 1;
        }
    };

    if !metadata.file_type().is_dir() {
        println!("{target}");
        return 0;
    }

    let mut entries = match std::fs::read_dir(path) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| show_all || !name.starts_with('.'))
            .collect::<Vec<_>>(),
        Err(error) => {
            eprintln!("ls: {target}: {error}");
            return 1;
        }
    };
    entries.sort();

    for entry in entries {
        println!("{entry}");
    }
    0
}

#[cfg(target_os = "wasi")]
fn mode_for_path(path: &str) -> u32 {
    unsafe { host_fs::path_mode(path.as_ptr(), path.len() as u32, 0) }
}

#[cfg(not(target_os = "wasi"))]
fn mode_for_path(path: &str) -> u32 {
    std::fs::symlink_metadata(path)
        .map(|metadata| metadata.permissions().mode())
        .unwrap_or(0)
}

fn render_mode_string(mode: u32) -> String {
    let mut rendered = String::with_capacity(9);
    for shift in [6, 3, 0] {
        rendered.push(if mode & (0o4 << shift) != 0 { 'r' } else { '-' });
        rendered.push(if mode & (0o2 << shift) != 0 { 'w' } else { '-' });
        rendered.push(if mode & (0o1 << shift) != 0 { 'x' } else { '-' });
    }
    rendered
}
