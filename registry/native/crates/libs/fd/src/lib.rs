//! Minimal fd-find compatible file finder for WASM.
//!
//! Uses std::fs for directory traversal (no walkdir/rayon dependency)
//! and regex for pattern matching. Covers common fd patterns:
//! fd PATTERN, fd -e EXT, fd -t f/d, fd -H (hidden), fd -I (no-ignore).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Entry point for fd command.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0]
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run(&str_args) {
        Ok(found) => {
            if found { 0 } else { 1 }
        }
        Err(msg) => {
            eprintln!("[fd error]: {}", msg);
            1
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TypeFilter {
    File,
    Directory,
    Symlink,
}

struct Options {
    pattern: Option<Regex>,
    extensions: Vec<String>,
    type_filter: Option<TypeFilter>,
    max_depth: Option<usize>,
    search_paths: Vec<String>,
    show_hidden: bool,
    _case_insensitive: bool,
    full_path: bool,
    absolute_path: bool,
}

fn run(args: &[String]) -> Result<bool, String> {
    let opts = parse_args(args)?;
    let mut found = false;

    for search_path in &opts.search_paths {
        let base = PathBuf::from(search_path);
        walk(&base, search_path, 0, &opts, &mut found)?;
    }

    Ok(found)
}

fn walk(
    full_path: &Path,
    base_path: &str,
    depth: usize,
    opts: &Options,
    found: &mut bool,
) -> Result<(), String> {
    if let Some(max) = opts.max_depth {
        if depth > max {
            return Ok(());
        }
    }

    // fd skips the root search directory itself (depth 0); only matches children
    if depth > 0 {
        if matches_entry(full_path, base_path, opts) {
            *found = true;
            let display = if opts.absolute_path {
                match fs::canonicalize(full_path) {
                    Ok(abs) => abs.to_string_lossy().to_string(),
                    Err(_) => full_path.to_string_lossy().to_string(),
                }
            } else {
                full_path.to_string_lossy().to_string()
            };
            println!("{}", display);
        }
    }

    // Recurse into directories
    if full_path.is_dir() {
        if let Some(max) = opts.max_depth {
            if depth >= max {
                return Ok(());
            }
        }

        let entries = fs::read_dir(full_path)
            .map_err(|e| format!("{}: {}", full_path.display(), e))?;

        let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        sorted.sort_by_key(|e| e.file_name());

        for entry in sorted {
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden files/directories unless -H is set
            if !opts.show_hidden && name.starts_with('.') {
                continue;
            }

            let child = entry.path();
            walk(&child, base_path, depth + 1, opts, found)?;
        }
    }

    Ok(())
}

fn matches_entry(path: &Path, _base_path: &str, opts: &Options) -> bool {
    // Type filter
    if let Some(ref tf) = opts.type_filter {
        let ok = match tf {
            TypeFilter::File => path.is_file(),
            TypeFilter::Directory => path.is_dir(),
            TypeFilter::Symlink => path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
        };
        if !ok {
            return false;
        }
    }

    // Extension filter
    if !opts.extensions.is_empty() {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let ext_lower = ext.to_lowercase();
        if !opts.extensions.iter().any(|e| e.to_lowercase() == ext_lower) {
            return false;
        }
    }

    // Pattern match
    if let Some(ref re) = opts.pattern {
        let target = if opts.full_path {
            path.to_string_lossy().to_string()
        } else {
            path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        if !re.is_match(&target) {
            return false;
        }
    }

    true
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut pattern: Option<String> = None;
    let mut extensions: Vec<String> = Vec::new();
    let mut type_filter: Option<TypeFilter> = None;
    let mut max_depth: Option<usize> = None;
    let mut search_paths: Vec<String> = Vec::new();
    let mut show_hidden = false;
    let mut case_insensitive = false;
    let mut full_path = false;
    let mut absolute_path = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("fd 0.1.0 (Agent OS)");
                std::process::exit(0);
            }
            "-H" | "--hidden" => {
                show_hidden = true;
            }
            "-I" | "--no-ignore" => {
                // No .gitignore support in WASM VFS — this is a no-op
            }
            "-i" | "--ignore-case" => {
                case_insensitive = true;
            }
            "-s" | "--case-sensitive" => {
                case_insensitive = false;
            }
            "-p" | "--full-path" => {
                full_path = true;
            }
            "-a" | "--absolute-path" => {
                absolute_path = true;
            }
            "-e" | "--extension" => {
                i += 1;
                if i >= args.len() {
                    return Err("-e/--extension requires an argument".to_string());
                }
                extensions.push(args[i].clone());
            }
            "-t" | "--type" => {
                i += 1;
                if i >= args.len() {
                    return Err("-t/--type requires an argument".to_string());
                }
                type_filter = Some(match args[i].as_str() {
                    "f" | "file" => TypeFilter::File,
                    "d" | "directory" => TypeFilter::Directory,
                    "l" | "symlink" => TypeFilter::Symlink,
                    other => return Err(format!("unknown type filter: {}", other)),
                });
            }
            "-d" | "--max-depth" => {
                i += 1;
                if i >= args.len() {
                    return Err("-d/--max-depth requires an argument".to_string());
                }
                max_depth = Some(
                    args[i]
                        .parse()
                        .map_err(|_| format!("invalid max-depth: {}", args[i]))?,
                );
            }
            arg if arg.starts_with('-') => {
                return Err(format!("unknown option: {}", arg));
            }
            _ => {
                // First positional arg is pattern, rest are search paths
                if pattern.is_none() {
                    pattern = Some(arg.clone());
                } else {
                    search_paths.push(arg.clone());
                }
            }
        }
        i += 1;
    }

    if search_paths.is_empty() {
        search_paths.push(".".to_string());
    }

    // Compile pattern regex
    let compiled_pattern = match pattern {
        Some(pat) => {
            let re_str = if case_insensitive {
                format!("(?i){}", pat)
            } else {
                pat
            };
            Some(Regex::new(&re_str).map_err(|e| format!("invalid pattern: {}", e))?)
        }
        None => None,
    };

    Ok(Options {
        pattern: compiled_pattern,
        extensions,
        type_filter,
        max_depth,
        search_paths,
        show_hidden,
        _case_insensitive: case_insensitive,
        full_path,
        absolute_path,
    })
}

fn print_help() {
    println!(
        "fd - a simple, fast file finder

USAGE:
    fd [OPTIONS] [PATTERN] [PATH...]

ARGS:
    <PATTERN>    Regex search pattern
    <PATH>...    Search paths (default: current directory)

OPTIONS:
    -H, --hidden          Include hidden files/directories
    -I, --no-ignore       Do not respect .gitignore (no-op in sandbox)
    -i, --ignore-case     Case-insensitive search
    -s, --case-sensitive  Case-sensitive search (default)
    -p, --full-path       Match pattern against full path
    -a, --absolute-path   Show absolute paths
    -e, --extension EXT   Filter by file extension
    -t, --type TYPE       Filter by type: f(ile), d(irectory), l(ink)
    -d, --max-depth N     Maximum search depth
    -h, --help            Print help
    -V, --version         Print version"
    );
}
