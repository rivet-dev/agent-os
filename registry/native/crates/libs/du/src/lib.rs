//! du -- estimate file space usage
//!
//! Recursive directory walk summing file sizes via std::fs::metadata.
//! Supports -s (summary), -h (human-readable), -a (all files), -c (grand total),
//! -d N (max depth).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut summary = false;
    let mut human = false;
    let mut all_files = false;
    let mut grand_total = false;
    let mut max_depth: Option<usize> = None;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 0;
    while i < str_args.len() {
        match str_args[i].as_str() {
            "-s" | "--summarize" => summary = true,
            "-h" | "--human-readable" => human = true,
            "-a" | "--all" => all_files = true,
            "-c" | "--total" => grand_total = true,
            "-d" | "--max-depth" => {
                i += 1;
                if i >= str_args.len() {
                    eprintln!("du: option '-d' requires an argument");
                    return 1;
                }
                match str_args[i].parse::<usize>() {
                    Ok(d) => max_depth = Some(d),
                    Err(_) => {
                        eprintln!("du: invalid maximum depth '{}'", str_args[i]);
                        return 1;
                    }
                }
            }
            s if s.starts_with("-d") && s.len() > 2 => match s[2..].parse::<usize>() {
                Ok(d) => max_depth = Some(d),
                Err(_) => {
                    eprintln!("du: invalid maximum depth '{}'", &s[2..]);
                    return 1;
                }
            },
            // Combined short flags like -sh, -shc
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for ch in s[1..].chars() {
                    match ch {
                        's' => summary = true,
                        'h' => human = true,
                        'a' => all_files = true,
                        'c' => grand_total = true,
                        _ => {
                            eprintln!("du: unknown option '{}'", s);
                            return 1;
                        }
                    }
                }
            }
            _ => paths.push(str_args[i].clone()),
        }
        i += 1;
    }

    if paths.is_empty() {
        paths.push(".".to_string());
    }

    // -s implies max_depth=0
    if summary {
        max_depth = Some(0);
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut total: u64 = 0;
    let mut exit_code = 0;

    for path in &paths {
        match walk_du(Path::new(path), 0, max_depth, all_files, human, &mut out) {
            Ok(size) => {
                total += size;
            }
            Err(e) => {
                eprintln!("du: {}: {}", path, e);
                exit_code = 1;
            }
        }
    }

    if grand_total {
        print_size(&mut out, total, human, "total");
    }

    exit_code
}

fn walk_du<W: Write>(
    path: &Path,
    depth: usize,
    max_depth: Option<usize>,
    all_files: bool,
    human: bool,
    out: &mut W,
) -> io::Result<u64> {
    let meta = fs::metadata(path)?;

    if meta.is_file() {
        let size = meta.len();
        // Convert to 1K blocks (like du default)
        let blocks = (size + 1023) / 1024;
        if all_files || depth == 0 {
            print_size(out, blocks, human, &path.to_string_lossy());
        }
        return Ok(blocks);
    }

    if meta.is_dir() {
        let mut dir_total: u64 = 0;
        match fs::read_dir(path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(e) => {
                            let child_path = e.path();
                            let child_meta = match fs::metadata(&child_path) {
                                Ok(m) => m,
                                Err(err) => {
                                    eprintln!("du: {}: {}", child_path.display(), err);
                                    continue;
                                }
                            };

                            if child_meta.is_dir() {
                                match walk_du(
                                    &child_path,
                                    depth + 1,
                                    max_depth,
                                    all_files,
                                    human,
                                    out,
                                ) {
                                    Ok(sub) => dir_total += sub,
                                    Err(err) => {
                                        eprintln!("du: {}: {}", child_path.display(), err);
                                    }
                                }
                            } else {
                                let size = child_meta.len();
                                let blocks = (size + 1023) / 1024;
                                dir_total += blocks;
                                if all_files {
                                    if let Some(md) = max_depth {
                                        if depth + 1 <= md {
                                            print_size(
                                                out,
                                                blocks,
                                                human,
                                                &child_path.to_string_lossy(),
                                            );
                                        }
                                    } else {
                                        print_size(
                                            out,
                                            blocks,
                                            human,
                                            &child_path.to_string_lossy(),
                                        );
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("du: {}: {}", path.display(), err);
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!("du: {}: {}", path.display(), err);
            }
        }

        // Print this directory's total if within depth limit
        if let Some(md) = max_depth {
            if depth <= md {
                print_size(out, dir_total, human, &path.to_string_lossy());
            }
        } else {
            print_size(out, dir_total, human, &path.to_string_lossy());
        }

        return Ok(dir_total);
    }

    // Symlinks, special files -- just report their metadata size
    let size = meta.len();
    let blocks = (size + 1023) / 1024;
    if all_files || depth == 0 {
        print_size(out, blocks, human, &path.to_string_lossy());
    }
    Ok(blocks)
}

fn print_size<W: Write>(out: &mut W, blocks: u64, human: bool, name: &str) {
    if human {
        let _ = writeln!(out, "{}\t{}", format_human(blocks), name);
    } else {
        let _ = writeln!(out, "{}\t{}", blocks, name);
    }
}

fn format_human(blocks: u64) -> String {
    let kb = blocks as f64;
    if kb < 1024.0 {
        format!("{}K", blocks)
    } else if kb < 1024.0 * 1024.0 {
        let mb = kb / 1024.0;
        if mb >= 10.0 {
            format!("{:.0}M", mb)
        } else {
            format!("{:.1}M", mb)
        }
    } else {
        let gb = kb / (1024.0 * 1024.0);
        if gb >= 10.0 {
            format!("{:.0}G", gb)
        } else {
            format!("{:.1}G", gb)
        }
    }
}
