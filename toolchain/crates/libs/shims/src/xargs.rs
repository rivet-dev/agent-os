//! Shim implementation of the `xargs` command.
//!
//! Reads items from stdin and executes a command with those items as arguments.
//! Uses std::process::Command (which delegates to wasi-ext proc_spawn).
//!
//! Usage:
//!   xargs [OPTION]... [COMMAND [INITIAL-ARGS]...]
//!
//! Options:
//!   -0, --null           Input items terminated by NUL, not whitespace
//!   -n N, --max-args=N   Use at most N arguments per invocation
//!   -I REPLSTR           Replace REPLSTR in COMMAND with input line
//!   -t, --verbose        Print command to stderr before executing
//!   -r, --no-run-if-empty  Do not run command if stdin is empty

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::Stdio;

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;
const MAX_INPUT_ITEMS: usize = 100_000;
const MAX_INPUT_ITEM_BYTES: usize = 1024 * 1024;

pub fn xargs(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0] ("xargs")
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut null_delim = false;
    let mut max_args: Option<usize> = None;
    let mut replace_str: Option<String> = None;
    let mut trace = false;
    let mut no_run_if_empty = false;
    let mut arg_file: Option<String> = None;
    let mut cmd_start = None;

    let mut i = 0;
    while i < str_args.len() {
        let arg = &str_args[i];
        match arg.as_str() {
            "-0" | "--null" => {
                null_delim = true;
                i += 1;
            }
            "-t" | "--verbose" => {
                trace = true;
                i += 1;
            }
            "-r" | "--no-run-if-empty" => {
                no_run_if_empty = true;
                i += 1;
            }
            "-a" | "--arg-file" => {
                i += 1;
                if i < str_args.len() {
                    arg_file = Some(str_args[i].clone());
                    i += 1;
                } else {
                    eprintln!("xargs: option requires an argument -- 'a'");
                    return 1;
                }
            }
            "-n" | "--max-args" => {
                i += 1;
                if i < str_args.len() {
                    match str_args[i].parse::<usize>() {
                        Ok(n) if n > 0 => max_args = Some(n),
                        _ => {
                            eprintln!("xargs: invalid number for -n: '{}'", str_args[i]);
                            return 1;
                        }
                    }
                    i += 1;
                } else {
                    eprintln!("xargs: option requires an argument -- 'n'");
                    return 1;
                }
            }
            "-I" => {
                i += 1;
                if i < str_args.len() {
                    replace_str = Some(str_args[i].clone());
                    i += 1;
                } else {
                    eprintln!("xargs: option requires an argument -- 'I'");
                    return 1;
                }
            }
            _ => {
                // Check for --max-args=N form
                if let Some(rest) = arg.strip_prefix("--max-args=") {
                    match rest.parse::<usize>() {
                        Ok(n) if n > 0 => max_args = Some(n),
                        _ => {
                            eprintln!("xargs: invalid number for --max-args: '{}'", rest);
                            return 1;
                        }
                    }
                    i += 1;
                } else if let Some(rest) = arg.strip_prefix("--arg-file=") {
                    if rest.is_empty() {
                        eprintln!("xargs: option requires an argument -- 'a'");
                        return 1;
                    }
                    arg_file = Some(rest.to_string());
                    i += 1;
                } else if arg.starts_with('-') && !arg.starts_with("--") && arg.len() > 2 {
                    // Handle -nN combined form
                    if let Some(rest) = arg.strip_prefix("-n") {
                        match rest.parse::<usize>() {
                            Ok(n) if n > 0 => max_args = Some(n),
                            _ => {
                                eprintln!("xargs: invalid number for -n: '{}'", rest);
                                return 1;
                            }
                        }
                        i += 1;
                    } else if let Some(rest) = arg.strip_prefix("-I") {
                        replace_str = Some(rest.to_string());
                        i += 1;
                    } else {
                        cmd_start = Some(i);
                        break;
                    }
                } else {
                    cmd_start = Some(i);
                    break;
                }
            }
        }
    }

    // Command and initial args
    let (program, initial_args) = if let Some(idx) = cmd_start {
        (str_args[idx].clone(), str_args[idx + 1..].to_vec())
    } else {
        ("echo".to_string(), Vec::new())
    };

    // Read all input from stdin
    let input_items = if null_delim {
        read_null_delimited(arg_file.as_deref())
    } else {
        read_whitespace_delimited(arg_file.as_deref())
    };

    let items = match input_items {
        Ok(items) => items,
        Err(e) => {
            eprintln!("xargs: {}", e);
            return 1;
        }
    };

    if items.is_empty() && no_run_if_empty {
        return 0;
    }

    // -I mode: one invocation per input item, replace occurrences
    if let Some(ref repl) = replace_str {
        let mut exit_code = 0;
        for item in &items {
            let replaced_args: Vec<String> = initial_args
                .iter()
                .map(|a| a.replace(repl.as_str(), item))
                .collect();

            let code = run_command(&program, &replaced_args, trace);
            if code != 0 {
                exit_code = code;
            }
        }
        return exit_code;
    }

    // Normal mode: batch items into invocations
    let batch_size = max_args.unwrap_or(items.len().max(1));
    let mut exit_code = 0;

    for chunk in items.chunks(batch_size) {
        let mut all_args = initial_args.clone();
        all_args.extend(chunk.iter().cloned());

        let code = run_command(&program, &all_args, trace);
        if code != 0 {
            exit_code = code;
        }
    }

    // If no items and no -r flag, run command once with just initial args
    if items.is_empty() && !no_run_if_empty {
        exit_code = run_command(&program, &initial_args, trace);
    }

    exit_code
}

/// Run a command with given arguments, optionally printing it to stderr.
fn run_command(program: &str, args: &[String], trace: bool) -> i32 {
    if trace {
        let mut cmd_line = program.to_string();
        for a in args {
            cmd_line.push(' ');
            cmd_line.push_str(a);
        }
        eprintln!("{}", cmd_line);
    }

    if program == "echo" {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        if let Err(e) = writeln!(out, "{}", args.join(" ")).and_then(|_| out.flush()) {
            eprintln!("xargs: {}", e);
            return 1;
        }
        return 0;
    }

    let mut cmd = std::process::Command::new(crate::which::resolve_program(program));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("xargs: {}: {}", program, e);
            127
        }
    }
}

/// Read NUL-delimited items from stdin.
fn read_null_delimited(arg_file: Option<&str>) -> io::Result<Vec<String>> {
    let input = match arg_file {
        Some(path) => read_limited_bytes(File::open(path)?)?,
        None => read_limited_bytes(io::stdin().lock())?,
    };

    let mut items = Vec::new();
    for segment in input.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        push_item(&mut items, String::from_utf8_lossy(segment).to_string())?;
    }
    Ok(items)
}

/// Read whitespace-delimited items from stdin, respecting shell quoting.
fn read_whitespace_delimited(arg_file: Option<&str>) -> io::Result<Vec<String>> {
    let input = match arg_file {
        Some(path) => read_limited_string(File::open(path)?)?,
        None => read_limited_string(io::stdin().lock())?,
    };

    let mut items = Vec::new();
    for line in input.lines() {
        for item in parse_quoted_args(line) {
            push_item(&mut items, item)?;
        }
    }
    Ok(items)
}

fn read_limited_bytes<R: Read>(reader: R) -> io::Result<Vec<u8>> {
    let mut input = Vec::new();
    let mut limited = reader.take((MAX_INPUT_BYTES + 1) as u64);
    limited.read_to_end(&mut input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input exceeds size limit",
        ));
    }
    Ok(input)
}

fn read_limited_string<R: Read>(mut reader: R) -> io::Result<String> {
    let mut input = String::new();
    let mut limited = reader.by_ref().take((MAX_INPUT_BYTES + 1) as u64);
    limited.read_to_string(&mut input)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input exceeds size limit",
        ));
    }
    Ok(input)
}

fn push_item(items: &mut Vec<String>, item: String) -> io::Result<()> {
    if item.len() > MAX_INPUT_ITEM_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "input item exceeds size limit",
        ));
    }
    if items.len() >= MAX_INPUT_ITEMS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many input items",
        ));
    }
    items.push(item);
    Ok(())
}

/// Parse a line respecting single quotes, double quotes, and backslash escapes.
fn parse_quoted_args(input: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut has_content = false;

    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            has_content = true;
            continue;
        }

        if in_single {
            if ch == '\'' {
                in_single = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        if in_double {
            if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_double = false;
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\\' => {
                escape = true;
                has_content = true;
            }
            '\'' => {
                in_single = true;
                has_content = true;
            }
            '"' => {
                in_double = true;
                has_content = true;
            }
            ' ' | '\t' => {
                if has_content {
                    items.push(current.clone());
                    current.clear();
                    has_content = false;
                }
            }
            _ => {
                current.push(ch);
                has_content = true;
            }
        }
    }

    if has_content {
        items.push(current);
    }

    items
}
