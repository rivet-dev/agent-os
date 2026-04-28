//! strings -- find printable ASCII strings in binary data

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};

pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut min_len: usize = 4;
    let mut offset_format: Option<char> = None; // 'd', 'o', or 'x'
    let mut filenames: Vec<String> = Vec::new();

    let mut i = 0;
    while i < str_args.len() {
        match str_args[i].as_str() {
            "-n" => {
                i += 1;
                if i >= str_args.len() {
                    eprintln!("strings: option '-n' requires an argument");
                    return 1;
                }
                match str_args[i].parse::<usize>() {
                    Ok(n) if n > 0 => min_len = n,
                    _ => {
                        eprintln!("strings: invalid minimum string length '{}'", str_args[i]);
                        return 1;
                    }
                }
            }
            s if s.starts_with("-n") => {
                let val = &s[2..];
                match val.parse::<usize>() {
                    Ok(n) if n > 0 => min_len = n,
                    _ => {
                        eprintln!("strings: invalid minimum string length '{}'", val);
                        return 1;
                    }
                }
            }
            "-t" => {
                i += 1;
                if i >= str_args.len() {
                    eprintln!("strings: option '-t' requires an argument");
                    return 1;
                }
                match str_args[i].as_str() {
                    "d" | "o" | "x" => offset_format = Some(str_args[i].chars().next().unwrap()),
                    _ => {
                        eprintln!("strings: invalid radix for -t: '{}'", str_args[i]);
                        return 1;
                    }
                }
            }
            s if s.starts_with('-') && s.len() > 1 => {
                // Try parsing as -N (numeric min length, GNU extension)
                if let Ok(n) = s[1..].parse::<usize>() {
                    if n > 0 {
                        min_len = n;
                    }
                } else {
                    eprintln!("strings: unknown option '{}'", s);
                    return 1;
                }
            }
            _ => filenames.push(str_args[i].clone()),
        }
        i += 1;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if filenames.is_empty() {
        let mut data = Vec::new();
        if let Err(e) = io::stdin().lock().read_to_end(&mut data) {
            eprintln!("strings: stdin: {}", e);
            return 1;
        }
        extract_strings(&data, min_len, offset_format, &mut out);
    } else {
        for filename in &filenames {
            match File::open(filename) {
                Ok(mut f) => {
                    let mut data = Vec::new();
                    if let Err(e) = f.read_to_end(&mut data) {
                        eprintln!("strings: {}: {}", filename, e);
                        return 1;
                    }
                    extract_strings(&data, min_len, offset_format, &mut out);
                }
                Err(e) => {
                    eprintln!("strings: {}: {}", filename, e);
                    return 1;
                }
            }
        }
    }

    0
}

fn extract_strings<W: Write>(data: &[u8], min_len: usize, offset_fmt: Option<char>, out: &mut W) {
    let mut run_start: Option<usize> = None;
    let mut run = Vec::new();

    for (i, &b) in data.iter().enumerate() {
        if is_printable_ascii(b) {
            if run.is_empty() {
                run_start = Some(i);
            }
            run.push(b);
        } else {
            if run.len() >= min_len {
                emit_string(out, &run, run_start.unwrap_or(0), offset_fmt);
            }
            run.clear();
            run_start = None;
        }
    }
    // Flush trailing run
    if run.len() >= min_len {
        emit_string(out, &run, run_start.unwrap_or(0), offset_fmt);
    }
}

fn emit_string<W: Write>(out: &mut W, run: &[u8], offset: usize, offset_fmt: Option<char>) {
    if let Some(fmt) = offset_fmt {
        match fmt {
            'd' => {
                let _ = write!(out, "{:7} ", offset);
            }
            'o' => {
                let _ = write!(out, "{:7o} ", offset);
            }
            'x' => {
                let _ = write!(out, "{:7x} ", offset);
            }
            _ => {}
        }
    }
    let _ = out.write_all(run);
    let _ = writeln!(out);
}

fn is_printable_ascii(b: u8) -> bool {
    // Printable ASCII: space (0x20) through tilde (0x7E), plus tab (0x09)
    b == b'\t' || (b >= 0x20 && b <= 0x7E)
}
