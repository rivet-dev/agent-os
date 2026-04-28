//! grep implementation using the regex crate (ripgrep's pure Rust regex engine).
//!
//! Supports grep, egrep (grep -E), and fgrep (grep -F) modes.
//! Dispatches on argv[0] basename for standalone binary usage.

mod rg_cmd;

use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::mem::ManuallyDrop;
use std::os::fd::FromRawFd;
use std::path::Path;

use regex::Regex;

/// Unified grep entry point. Dispatches on argv[0]:
/// - "egrep" -> Extended mode
/// - "fgrep" -> Fixed mode
/// - default -> Basic mode
pub fn main(args: Vec<OsString>) -> i32 {
    let mode = match args.first().and_then(|a| Path::new(a).file_name()) {
        Some(name) if name == "egrep" => GrepMode::Extended,
        Some(name) if name == "fgrep" => GrepMode::Fixed,
        _ => GrepMode::Basic,
    };
    run_grep(args, mode)
}

/// Entry point for grep command (Basic mode).
pub fn grep(args: Vec<OsString>) -> i32 {
    run_grep(args, GrepMode::Basic)
}

/// Entry point for egrep command (Extended regex).
pub fn egrep(args: Vec<OsString>) -> i32 {
    run_grep(args, GrepMode::Extended)
}

/// Entry point for fgrep command (Fixed strings).
pub fn fgrep(args: Vec<OsString>) -> i32 {
    run_grep(args, GrepMode::Fixed)
}

/// Entry point for rg command.
pub fn rg(args: Vec<OsString>) -> i32 {
    rg_cmd::rg(args)
}

struct RawStdout;

impl Write for RawStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
        file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
        file.flush()
    }
}

/// grep mode determines how patterns are interpreted.
#[derive(Clone, Copy, PartialEq)]
enum GrepMode {
    /// Basic regular expressions (default grep)
    Basic,
    /// Extended regular expressions (egrep / grep -E)
    Extended,
    /// Fixed strings (fgrep / grep -F)
    Fixed,
}

struct GrepOptions {
    mode: GrepMode,
    ignore_case: bool,
    invert_match: bool,
    count_only: bool,
    files_with_matches: bool,
    files_without_matches: bool,
    line_numbers: bool,
    word_regexp: bool,
    line_regexp: bool,
    max_count: Option<usize>,
    quiet: bool,
    patterns: Vec<String>,
    files: Vec<String>,
}

impl GrepOptions {
    fn new(mode: GrepMode) -> Self {
        Self {
            mode,
            ignore_case: false,
            invert_match: false,
            count_only: false,
            files_with_matches: false,
            files_without_matches: false,
            line_numbers: false,
            word_regexp: false,
            line_regexp: false,
            max_count: None,
            quiet: false,
            patterns: Vec::new(),
            files: Vec::new(),
        }
    }
}

fn run_grep(args: Vec<OsString>, default_mode: GrepMode) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0]
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let opts = match parse_args(&str_args, default_mode) {
        Ok(opts) => opts,
        Err(msg) => {
            eprintln!("grep: {}", msg);
            return 2;
        }
    };

    if opts.patterns.is_empty() {
        eprintln!("grep: no pattern specified");
        return 2;
    }

    let regex = match build_regex(&opts) {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("grep: {}", msg);
            return 2;
        }
    };

    let multiple_files = opts.files.len() > 1;
    let mut any_match = false;

    if opts.files.is_empty() {
        // Read from stdin
        let stdin = io::stdin();
        let reader = stdin.lock();
        if search_reader(reader, None, &regex, &opts, multiple_files) {
            any_match = true;
        }
    } else {
        for file in &opts.files {
            if file == "-" {
                let stdin = io::stdin();
                let reader = stdin.lock();
                let label = if multiple_files {
                    Some("(standard input)")
                } else {
                    None
                };
                if search_reader(reader, label, &regex, &opts, multiple_files) {
                    any_match = true;
                }
            } else {
                match std::fs::File::open(file) {
                    Ok(f) => {
                        let reader = io::BufReader::new(f);
                        let label = if multiple_files {
                            Some(file.as_str())
                        } else {
                            None
                        };
                        if search_reader(reader, label, &regex, &opts, multiple_files) {
                            any_match = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("grep: {}: {}", file, e);
                    }
                }
            }
        }
    }

    if any_match {
        0
    } else {
        1
    }
}

fn parse_args(args: &[String], default_mode: GrepMode) -> Result<GrepOptions, String> {
    let mut opts = GrepOptions::new(default_mode);
    let mut i = 0;
    let mut pattern_from_args = false;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            i += 1;
            // Remaining args are files (or first is pattern if none yet)
            break;
        }

        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                match chars[j] {
                    'E' => opts.mode = GrepMode::Extended,
                    'F' => opts.mode = GrepMode::Fixed,
                    'G' => opts.mode = GrepMode::Basic,
                    'i' | 'y' => opts.ignore_case = true,
                    'v' => opts.invert_match = true,
                    'c' => opts.count_only = true,
                    'l' => opts.files_with_matches = true,
                    'L' => opts.files_without_matches = true,
                    'n' => opts.line_numbers = true,
                    'w' => opts.word_regexp = true,
                    'x' => opts.line_regexp = true,
                    'q' | 's' => opts.quiet = true,
                    'h' => {} // suppress filename (handled by multiple_files logic)
                    'H' => {} // force filename
                    'e' => {
                        // -e PATTERN (rest of this flag group or next arg)
                        let rest: String = chars[j + 1..].iter().collect();
                        if !rest.is_empty() {
                            opts.patterns.push(rest);
                            pattern_from_args = true;
                            j = chars.len(); // consumed rest
                            continue;
                        } else {
                            i += 1;
                            if i >= args.len() {
                                return Err("option requires an argument -- 'e'".to_string());
                            }
                            opts.patterns.push(args[i].clone());
                            pattern_from_args = true;
                            j = chars.len();
                            continue;
                        }
                    }
                    'f' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'f'".to_string());
                        }
                        match std::fs::read_to_string(&args[i]) {
                            Ok(content) => {
                                for line in content.lines() {
                                    if !line.is_empty() {
                                        opts.patterns.push(line.to_string());
                                    }
                                }
                                pattern_from_args = true;
                            }
                            Err(e) => return Err(format!("{}: {}", args[i], e)),
                        }
                        j = chars.len();
                        continue;
                    }
                    'm' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'm'".to_string());
                        }
                        opts.max_count = Some(
                            args[i]
                                .parse()
                                .map_err(|_| format!("invalid max count '{}'", args[i]))?,
                        );
                        j = chars.len();
                        continue;
                    }
                    _ => {
                        return Err(format!("invalid option -- '{}'", chars[j]));
                    }
                }
                j += 1;
            }
        } else if arg.starts_with("--") {
            match arg.as_str() {
                "--extended-regexp" => opts.mode = GrepMode::Extended,
                "--fixed-strings" => opts.mode = GrepMode::Fixed,
                "--basic-regexp" => opts.mode = GrepMode::Basic,
                "--ignore-case" => opts.ignore_case = true,
                "--invert-match" => opts.invert_match = true,
                "--count" => opts.count_only = true,
                "--files-with-matches" => opts.files_with_matches = true,
                "--files-without-match" => opts.files_without_matches = true,
                "--line-number" => opts.line_numbers = true,
                "--word-regexp" => opts.word_regexp = true,
                "--line-regexp" => opts.line_regexp = true,
                "--quiet" | "--silent" => opts.quiet = true,
                _ if arg.starts_with("--regexp=") => {
                    opts.patterns.push(arg[9..].to_string());
                    pattern_from_args = true;
                }
                _ if arg.starts_with("--max-count=") => {
                    opts.max_count = Some(
                        arg[12..]
                            .parse()
                            .map_err(|_| format!("invalid max count '{}'", &arg[12..]))?,
                    );
                }
                _ => {
                    return Err(format!("unrecognized option '{}'", arg));
                }
            }
        } else {
            // Positional argument: first is pattern (if no -e), rest are files
            if !pattern_from_args && opts.patterns.is_empty() {
                opts.patterns.push(arg.clone());
                pattern_from_args = true;
            } else {
                opts.files.push(arg.clone());
            }
        }
        i += 1;
    }

    // Remaining args after --
    while i < args.len() {
        if !pattern_from_args && opts.patterns.is_empty() {
            opts.patterns.push(args[i].clone());
            pattern_from_args = true;
        } else {
            opts.files.push(args[i].clone());
        }
        i += 1;
    }

    Ok(opts)
}

/// Build a compiled regex from the grep options.
fn build_regex(opts: &GrepOptions) -> Result<Regex, String> {
    let pattern = if opts.patterns.len() == 1 {
        build_single_pattern(&opts.patterns[0], opts)
    } else {
        // Multiple patterns: combine with alternation
        let parts: Vec<String> = opts
            .patterns
            .iter()
            .map(|p| format!("(?:{})", build_single_pattern(p, opts)))
            .collect();
        parts.join("|")
    };

    let mut builder = regex::RegexBuilder::new(&pattern);
    builder.case_insensitive(opts.ignore_case);

    builder
        .build()
        .map_err(|e| format!("invalid pattern: {}", e))
}

/// Convert a single pattern string to a regex pattern based on mode.
fn build_single_pattern(pattern: &str, opts: &GrepOptions) -> String {
    let base = match opts.mode {
        GrepMode::Fixed => regex::escape(pattern),
        GrepMode::Basic => convert_bre_to_ere(pattern),
        GrepMode::Extended => pattern.to_string(),
    };

    let wrapped = if opts.word_regexp {
        format!(r"\b(?:{})\b", base)
    } else if opts.line_regexp {
        format!("^(?:{})$", base)
    } else {
        base
    };

    wrapped
}

/// Convert POSIX Basic Regular Expression to Extended (Rust regex syntax).
/// In BRE: \(, \), \{, \}, \+, \?, \| are special; unescaped versions are literal.
/// In ERE (and Rust regex): (, ), {, }, +, ?, | are special without backslash.
fn convert_bre_to_ere(bre: &str) -> String {
    let mut result = String::with_capacity(bre.len());
    let chars: Vec<char> = bre.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                '(' => {
                    result.push('(');
                    i += 2;
                }
                ')' => {
                    result.push(')');
                    i += 2;
                }
                '{' => {
                    result.push('{');
                    i += 2;
                }
                '}' => {
                    result.push('}');
                    i += 2;
                }
                '+' => {
                    result.push('+');
                    i += 2;
                }
                '?' => {
                    result.push('?');
                    i += 2;
                }
                '|' => {
                    result.push('|');
                    i += 2;
                }
                '1'..='9' => {
                    // Backreference - not supported in Rust regex, pass through
                    result.push('\\');
                    result.push(chars[i + 1]);
                    i += 2;
                }
                _ => {
                    result.push('\\');
                    result.push(chars[i + 1]);
                    i += 2;
                }
            }
        } else {
            match chars[i] {
                // In BRE, unescaped (, ), {, }, +, ? are literal
                '(' => {
                    result.push_str("\\(");
                    i += 1;
                }
                ')' => {
                    result.push_str("\\)");
                    i += 1;
                }
                '{' => {
                    result.push_str("\\{");
                    i += 1;
                }
                '}' => {
                    result.push_str("\\}");
                    i += 1;
                }
                _ => {
                    result.push(chars[i]);
                    i += 1;
                }
            }
        }
    }

    result
}

/// Search a reader for matching lines. Returns true if any match was found.
fn search_reader<R: Read>(
    reader: R,
    filename: Option<&str>,
    regex: &Regex,
    opts: &GrepOptions,
    show_filename: bool,
) -> bool {
    let buf_reader = io::BufReader::new(reader);
    let mut match_count: usize = 0;
    let mut line_num: usize = 0;
    let mut out = RawStdout;

    for line_result in buf_reader.lines() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        line_num += 1;

        let is_match = regex.is_match(&line);
        let is_match = if opts.invert_match {
            !is_match
        } else {
            is_match
        };

        if is_match {
            match_count += 1;

            if opts.quiet {
                return true;
            }

            if opts.files_with_matches {
                if let Some(name) = filename {
                    let _ = writeln!(out, "{}", name);
                } else {
                    let _ = writeln!(out, "(standard input)");
                }
                return true;
            }

            if !opts.count_only && !opts.files_without_matches {
                let prefix = match (show_filename, filename, opts.line_numbers) {
                    (true, Some(name), true) => format!("{}:{}:", name, line_num),
                    (true, Some(name), false) => format!("{}:", name),
                    (_, _, true) => format!("{}:", line_num),
                    _ => String::new(),
                };
                let _ = writeln!(out, "{}{}", prefix, line);
            }

            if let Some(max) = opts.max_count {
                if match_count >= max {
                    break;
                }
            }
        }
    }

    if opts.count_only && !opts.quiet {
        if show_filename {
            if let Some(name) = filename {
                let _ = writeln!(out, "{}:{}", name, match_count);
            } else {
                let _ = writeln!(out, "{}", match_count);
            }
        } else {
            let _ = writeln!(out, "{}", match_count);
        }
    }

    if opts.files_without_matches && match_count == 0 {
        if let Some(name) = filename {
            let _ = writeln!(out, "{}", name);
        } else {
            let _ = writeln!(out, "(standard input)");
        }
    }

    let _ = out.flush();

    match_count > 0
}
