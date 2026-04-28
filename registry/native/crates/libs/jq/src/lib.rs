//! jq implementation using the jaq crate (pure Rust, jq-compatible).
//!
//! Wraps jaq-core/jaq-std/jaq-json to provide a standard jq CLI interface.

use std::ffi::OsString;
use std::fs::File as StdFile;
use std::io::{self, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;

use jaq_core::load::{Arena, File, Loader};
use jaq_core::{Compiler, Ctx, RcIter};
use jaq_json::Val;

/// Entry point for jq command.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0]
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run_jq(&str_args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("jq: {}", msg);
            2
        }
    }
}

struct RawStdout;

impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file = ManuallyDrop::new(unsafe { StdFile::from_raw_fd(1) });
        file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = ManuallyDrop::new(unsafe { StdFile::from_raw_fd(1) });
        file.flush()
    }
}

struct JqOptions {
    filter: String,
    raw_output: bool,
    raw_input: bool,
    slurp: bool,
    compact: bool,
    null_input: bool,
    exit_status: bool,
    join_output: bool,
    args: Vec<(String, String)>,
    jsonargs: Vec<(String, Val)>,
}

fn parse_args(args: &[String]) -> Result<JqOptions, String> {
    let mut opts = JqOptions {
        filter: String::new(),
        raw_output: false,
        raw_input: false,
        slurp: false,
        compact: false,
        null_input: false,
        exit_status: false,
        join_output: false,
        args: Vec::new(),
        jsonargs: Vec::new(),
    };

    let mut filter_set = false;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            break;
        }

        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            // Handle combined short flags like -rn, -crS, etc.
            let flags = &arg[1..];
            for c in flags.chars() {
                match c {
                    'r' => opts.raw_output = true,
                    'R' => opts.raw_input = true,
                    's' => opts.slurp = true,
                    'c' => opts.compact = true,
                    'n' => opts.null_input = true,
                    'e' => opts.exit_status = true,
                    'j' => opts.join_output = true,
                    'S' => {} // sort keys - jaq sorts by default
                    _ => return Err(format!("unknown option: -{}", c)),
                }
            }
        } else if arg == "--raw-output" || arg == "--raw-output0" {
            opts.raw_output = true;
        } else if arg == "--raw-input" {
            opts.raw_input = true;
        } else if arg == "--slurp" {
            opts.slurp = true;
        } else if arg == "--compact-output" {
            opts.compact = true;
        } else if arg == "--null-input" {
            opts.null_input = true;
        } else if arg == "--exit-status" {
            opts.exit_status = true;
        } else if arg == "--join-output" {
            opts.join_output = true;
        } else if arg == "--arg" {
            if i + 2 >= args.len() {
                return Err("--arg requires name and value".to_string());
            }
            let name = args[i + 1].clone();
            let value = args[i + 2].clone();
            opts.args.push((name, value));
            i += 2;
        } else if arg == "--argjson" {
            if i + 2 >= args.len() {
                return Err("--argjson requires name and value".to_string());
            }
            let name = args[i + 1].clone();
            let json_str = &args[i + 2];
            let value: serde_json::Value = serde_json::from_str(json_str)
                .map_err(|e| format!("invalid JSON for --argjson: {}", e))?;
            opts.jsonargs.push((name, Val::from(value)));
            i += 2;
        } else if !filter_set {
            opts.filter = arg.clone();
            filter_set = true;
        } else {
            return Err(format!("unexpected argument: {}", arg));
        }

        i += 1;
    }

    if !filter_set {
        return Err("no filter provided".to_string());
    }

    Ok(opts)
}

fn read_inputs(opts: &JqOptions) -> Result<Vec<Val>, String> {
    let mut stdin_data = String::new();
    io::stdin()
        .read_to_string(&mut stdin_data)
        .map_err(|e| format!("failed to read stdin: {}", e))?;

    if opts.null_input {
        return Ok(vec![Val::from(serde_json::Value::Null)]);
    }

    if opts.raw_input {
        if opts.slurp {
            let arr: Vec<serde_json::Value> = stdin_data
                .lines()
                .map(|l| serde_json::Value::String(l.to_string()))
                .collect();
            Ok(vec![Val::from(serde_json::Value::Array(arr))])
        } else {
            let lines: Vec<Val> = stdin_data
                .lines()
                .map(|line| Val::from(serde_json::Value::String(line.to_string())))
                .collect();
            Ok(lines)
        }
    } else {
        let trimmed = stdin_data.trim();
        if trimmed.is_empty() {
            return Ok(vec![Val::from(serde_json::Value::Null)]);
        }

        let mut values = Vec::new();
        let decoder = serde_json::Deserializer::from_str(trimmed).into_iter::<serde_json::Value>();
        for result in decoder {
            let value = result.map_err(|e| format!("invalid JSON input: {}", e))?;
            values.push(Val::from(value));
        }

        if opts.slurp {
            let arr = serde_json::Value::Array(
                values
                    .into_iter()
                    .map(|v| {
                        serde_json::from_str(&format!("{}", v)).unwrap_or(serde_json::Value::Null)
                    })
                    .collect(),
            );
            Ok(vec![Val::from(arr)])
        } else {
            Ok(values)
        }
    }
}

/// Format a jaq Val as a string for output.
fn format_output(val: &Val, opts: &JqOptions) -> String {
    let compact_str = format!("{}", val);

    // For raw output, unquote strings
    if opts.raw_output {
        if compact_str.starts_with('"') && compact_str.ends_with('"') && compact_str.len() >= 2 {
            if let Ok(unescaped) = serde_json::from_str::<String>(&compact_str) {
                return unescaped;
            }
        }
    }

    if opts.compact {
        compact_str
    } else {
        // Pretty print via serde_json
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&compact_str) {
            serde_json::to_string_pretty(&v).unwrap_or(compact_str)
        } else {
            compact_str
        }
    }
}

fn run_jq(args: &[String]) -> Result<i32, String> {
    let opts = parse_args(args)?;
    let inputs = read_inputs(&opts)?;

    // Set up variable bindings for --arg and --argjson
    let mut var_vals: Vec<Val> = Vec::new();

    for (_name, value) in &opts.args {
        var_vals.push(Val::from(serde_json::Value::String(value.clone())));
    }
    for (_name, value) in &opts.jsonargs {
        var_vals.push(value.clone());
    }

    // Load and compile filter
    let loader = Loader::new(jaq_std::defs().chain(jaq_json::defs()));
    let arena = Arena::default();
    let program = File {
        code: opts.filter.as_str(),
        path: (),
    };
    let modules = loader
        .load(&arena, program)
        .map_err(|errs| format!("parse error: {:?}", errs))?;

    let filter = Compiler::default()
        .with_funs(jaq_std::funs().chain(jaq_json::funs()))
        .compile(modules)
        .map_err(|errs| format!("compile error: {:?}", errs))?;

    let empty_inputs = RcIter::new(core::iter::empty());
    let mut out = RawStdout;
    let mut had_false_or_null = false;

    for input in inputs {
        let ctx = Ctx::new(var_vals.iter().cloned(), &empty_inputs);
        let results = filter.run((ctx, input));

        for result in results {
            match result {
                Ok(val) => {
                    let s = format_output(&val, &opts);

                    // Track for --exit-status
                    let compact = format!("{}", val);
                    if compact == "null" || compact == "false" {
                        had_false_or_null = true;
                    }

                    if opts.join_output {
                        write!(out, "{}", s).ok();
                    } else {
                        writeln!(out, "{}", s).ok();
                    }
                }
                Err(e) => {
                    eprintln!("jq: error: {}", e);
                    return Ok(5);
                }
            }
        }
    }

    if opts.exit_status && had_false_or_null {
        return Ok(1);
    }

    out.flush().ok();

    Ok(0)
}
