//! POSIX find implementation for WASM.
//!
//! Custom implementation using std::fs (WASI) for directory traversal
//! and glob-to-regex conversion for pattern matching. fd-find crate
//! cannot be used directly as it's a binary crate with platform-specific
//! dependencies incompatible with wasm32-wasip1.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

/// Entry point for find command.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1) // skip argv[0]
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run_find(&str_args) {
        Ok(found) => {
            if found {
                0
            } else {
                0
            } // find returns 0 even if no matches
        }
        Err(msg) => {
            eprintln!("find: {}", msg);
            1
        }
    }
}

/// Parsed find expression node.
#[derive(Debug)]
enum Expr {
    /// -name PATTERN (glob match on filename only)
    Name(Regex),
    /// -iname PATTERN (case-insensitive glob match on filename)
    IName(Regex),
    /// -path PATTERN (glob match on full path)
    PathMatch(Regex),
    /// -ipath PATTERN (case-insensitive glob match on full path)
    IPathMatch(Regex),
    /// -type TYPE (d, f, l)
    Type(FileType),
    /// -empty (file size 0 or empty directory)
    Empty,
    /// -maxdepth N (handled specially during traversal)
    MaxDepth(usize),
    /// -mindepth N (handled specially during traversal)
    MinDepth(usize),
    /// -print (explicit print action)
    Print,
    /// -not EXPR / ! EXPR
    Not(Box<Expr>),
    /// EXPR -a EXPR / EXPR -and EXPR / implicit AND
    And(Box<Expr>, Box<Expr>),
    /// EXPR -o EXPR / EXPR -or EXPR
    Or(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum FileType {
    Directory,
    File,
    Symlink,
}

struct FindOptions {
    paths: Vec<String>,
    expr: Option<Expr>,
    max_depth: Option<usize>,
    min_depth: usize,
}

fn run_find(args: &[String]) -> Result<bool, String> {
    let opts = parse_args(args)?;
    let mut found_any = false;

    for path in &opts.paths {
        walk(&PathBuf::from(path), path, 0, &opts, &mut found_any)?;
    }

    Ok(found_any)
}

/// Recursive directory walk.
fn walk(
    full_path: &Path,
    display_path: &str,
    depth: usize,
    opts: &FindOptions,
    found_any: &mut bool,
) -> Result<(), String> {
    // Check max depth before processing
    if let Some(max) = opts.max_depth {
        if depth > max {
            return Ok(());
        }
    }

    // Evaluate expression against this entry (if at or below min_depth)
    if depth >= opts.min_depth {
        let matches = match &opts.expr {
            Some(expr) => eval_expr(expr, full_path, display_path),
            None => true, // no expression means match everything
        };

        if matches {
            *found_any = true;
            // Default action is print
            println!("{}", display_path);
        }
    }

    // Recurse into directories
    if full_path.is_dir() {
        // Check max depth for children
        if let Some(max) = opts.max_depth {
            if depth >= max {
                return Ok(());
            }
        }

        let entries = fs::read_dir(full_path).map_err(|e| format!("{}: {}", display_path, e))?;

        let mut sorted_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        sorted_entries.sort_by_key(|e| e.file_name());

        for entry in sorted_entries {
            let child_name = entry.file_name().to_string_lossy().to_string();
            let child_display = if display_path == "/" {
                format!("/{}", child_name)
            } else {
                format!("{}/{}", display_path, child_name)
            };
            let child_full = entry.path();

            walk(&child_full, &child_display, depth + 1, opts, found_any)?;
        }
    }

    Ok(())
}

/// Evaluate an expression against a path.
fn eval_expr(expr: &Expr, full_path: &Path, display_path: &str) -> bool {
    match expr {
        Expr::Name(re) => {
            let name = full_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            re.is_match(&name)
        }
        Expr::IName(re) => {
            let name = full_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            re.is_match(&name)
        }
        Expr::PathMatch(re) => re.is_match(display_path),
        Expr::IPathMatch(re) => re.is_match(display_path),
        Expr::Type(ft) => match ft {
            FileType::Directory => full_path.is_dir(),
            FileType::File => full_path.is_file(),
            FileType::Symlink => full_path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false),
        },
        Expr::Empty => {
            if full_path.is_dir() {
                fs::read_dir(full_path)
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(false)
            } else if full_path.is_file() {
                full_path.metadata().map(|m| m.len() == 0).unwrap_or(false)
            } else {
                false
            }
        }
        Expr::MaxDepth(_) | Expr::MinDepth(_) => true, // handled in walk()
        Expr::Print => true,                           // print is an action, always "matches"
        Expr::Not(inner) => !eval_expr(inner, full_path, display_path),
        Expr::And(left, right) => {
            eval_expr(left, full_path, display_path) && eval_expr(right, full_path, display_path)
        }
        Expr::Or(left, right) => {
            eval_expr(left, full_path, display_path) || eval_expr(right, full_path, display_path)
        }
    }
}

/// Convert a shell glob pattern to a regex pattern.
/// Supports *, ?, [charset], and ** (for path matching).
fn glob_to_regex(glob: &str, case_insensitive: bool) -> Result<Regex, String> {
    let mut re = String::from("^");
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    // ** matches everything including /
                    re.push_str(".*");
                    i += 1;
                } else {
                    // * matches everything except /
                    re.push_str("[^/]*");
                }
            }
            '?' => re.push_str("[^/]"),
            '[' => {
                re.push('[');
                i += 1;
                if i < chars.len() && chars[i] == '!' {
                    re.push('^');
                    i += 1;
                } else if i < chars.len() && chars[i] == '^' {
                    re.push('^');
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        re.push('\\');
                        i += 1;
                        re.push(chars[i]);
                    } else {
                        re.push(chars[i]);
                    }
                    i += 1;
                }
                re.push(']');
            }
            '.' | '+' | '(' | ')' | '{' | '}' | '|' | '^' | '$' | '\\' => {
                re.push('\\');
                re.push(chars[i]);
            }
            _ => re.push(chars[i]),
        }
        i += 1;
    }

    re.push('$');

    let pattern = if case_insensitive {
        format!("(?i){}", re)
    } else {
        re
    };

    Regex::new(&pattern).map_err(|e| format!("invalid pattern: {}", e))
}

/// Parse find command arguments.
fn parse_args(args: &[String]) -> Result<FindOptions, String> {
    let mut paths: Vec<String> = Vec::new();
    let mut exprs: Vec<Expr> = Vec::new();
    let mut max_depth: Option<usize> = None;
    let mut min_depth: usize = 0;
    let mut i = 0;

    // First, collect paths (arguments before any expression)
    while i < args.len() {
        let arg = &args[i];
        if arg.starts_with('-') || arg == "!" || arg == "(" {
            break;
        }
        paths.push(arg.clone());
        i += 1;
    }

    if paths.is_empty() {
        paths.push(".".to_string());
    }

    // Parse expressions
    while i < args.len() {
        let arg = &args[i];

        match arg.as_str() {
            "-name" => {
                i += 1;
                if i >= args.len() {
                    return Err("-name requires an argument".to_string());
                }
                let re = glob_to_regex(&args[i], false)?;
                exprs.push(Expr::Name(re));
            }
            "-iname" => {
                i += 1;
                if i >= args.len() {
                    return Err("-iname requires an argument".to_string());
                }
                let re = glob_to_regex(&args[i], true)?;
                exprs.push(Expr::IName(re));
            }
            "-path" | "-wholename" => {
                i += 1;
                if i >= args.len() {
                    return Err(format!("{} requires an argument", arg));
                }
                let re = glob_to_regex(&args[i], false)?;
                exprs.push(Expr::PathMatch(re));
            }
            "-ipath" | "-iwholename" => {
                i += 1;
                if i >= args.len() {
                    return Err(format!("{} requires an argument", arg));
                }
                let re = glob_to_regex(&args[i], true)?;
                exprs.push(Expr::IPathMatch(re));
            }
            "-type" => {
                i += 1;
                if i >= args.len() {
                    return Err("-type requires an argument".to_string());
                }
                let ft = match args[i].as_str() {
                    "d" => FileType::Directory,
                    "f" => FileType::File,
                    "l" => FileType::Symlink,
                    other => return Err(format!("unknown file type: {}", other)),
                };
                exprs.push(Expr::Type(ft));
            }
            "-empty" => {
                exprs.push(Expr::Empty);
            }
            "-maxdepth" => {
                i += 1;
                if i >= args.len() {
                    return Err("-maxdepth requires an argument".to_string());
                }
                let n: usize = args[i]
                    .parse()
                    .map_err(|_| format!("-maxdepth: {}: not a number", args[i]))?;
                max_depth = Some(n);
            }
            "-mindepth" => {
                i += 1;
                if i >= args.len() {
                    return Err("-mindepth requires an argument".to_string());
                }
                let n: usize = args[i]
                    .parse()
                    .map_err(|_| format!("-mindepth: {}: not a number", args[i]))?;
                min_depth = n;
            }
            "-print" => {
                exprs.push(Expr::Print);
            }
            "!" | "-not" => {
                i += 1;
                if i >= args.len() {
                    return Err("! requires an expression".to_string());
                }
                // Parse the next single expression
                let (next_expr, next_i) = parse_single_expr(&args, i)?;
                if let Some((md, mi)) = extract_depth(&next_expr) {
                    if let Some(d) = md {
                        max_depth = Some(d);
                    }
                    min_depth = mi.unwrap_or(min_depth);
                }
                exprs.push(Expr::Not(Box::new(next_expr)));
                i = next_i;
                continue; // skip the i += 1 at end
            }
            "-a" | "-and" => {
                // Implicit AND between consecutive expressions, -a is explicit
                // Just skip it; AND is applied when combining exprs
            }
            "-o" | "-or" => {
                // Combine everything so far as left side of OR
                if exprs.is_empty() {
                    return Err("-or requires a preceding expression".to_string());
                }
                let left = combine_and(exprs);

                // Parse remaining as right side
                i += 1;
                let mut right_exprs = Vec::new();
                while i < args.len() {
                    let (e, ni) = parse_single_expr(args, i)?;
                    if let Some((md, mi)) = extract_depth(&e) {
                        if let Some(d) = md {
                            max_depth = Some(d);
                        }
                        min_depth = mi.unwrap_or(min_depth);
                    }
                    right_exprs.push(e);
                    i = ni;
                }
                let right = combine_and(right_exprs);
                exprs = vec![Expr::Or(Box::new(left), Box::new(right))];
                break;
            }
            "(" => {
                // Grouped expression - find matching )
                i += 1;
                let mut depth = 1;
                let start = i;
                while i < args.len() && depth > 0 {
                    if args[i] == "(" {
                        depth += 1;
                    }
                    if args[i] == ")" {
                        depth -= 1;
                    }
                    if depth > 0 {
                        i += 1;
                    }
                }
                if depth != 0 {
                    return Err("unmatched '('".to_string());
                }
                let sub_args: Vec<String> = args[start..i].to_vec();
                let sub_opts = parse_args_exprs(&sub_args)?;
                if let Some(e) = sub_opts {
                    exprs.push(e);
                }
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {}", other));
            }
            _ => {
                // Treat as path if no expressions yet, otherwise error
                return Err(format!("unexpected argument: {}", arg));
            }
        }
        i += 1;
    }

    let expr = if exprs.is_empty() {
        None
    } else {
        Some(combine_and(exprs))
    };

    Ok(FindOptions {
        paths,
        expr,
        max_depth,
        min_depth,
    })
}

/// Parse a single expression at position i, returning the expression and the next position.
fn parse_single_expr(args: &[String], i: usize) -> Result<(Expr, usize), String> {
    if i >= args.len() {
        return Err("unexpected end of expression".to_string());
    }

    let arg = &args[i];
    match arg.as_str() {
        "-name" => {
            if i + 1 >= args.len() {
                return Err("-name requires an argument".to_string());
            }
            let re = glob_to_regex(&args[i + 1], false)?;
            Ok((Expr::Name(re), i + 2))
        }
        "-iname" => {
            if i + 1 >= args.len() {
                return Err("-iname requires an argument".to_string());
            }
            let re = glob_to_regex(&args[i + 1], true)?;
            Ok((Expr::IName(re), i + 2))
        }
        "-path" | "-wholename" => {
            if i + 1 >= args.len() {
                return Err(format!("{} requires an argument", arg));
            }
            let re = glob_to_regex(&args[i + 1], false)?;
            Ok((Expr::PathMatch(re), i + 2))
        }
        "-type" => {
            if i + 1 >= args.len() {
                return Err("-type requires an argument".to_string());
            }
            let ft = match args[i + 1].as_str() {
                "d" => FileType::Directory,
                "f" => FileType::File,
                "l" => FileType::Symlink,
                other => return Err(format!("unknown file type: {}", other)),
            };
            Ok((Expr::Type(ft), i + 2))
        }
        "-empty" => Ok((Expr::Empty, i + 1)),
        "-maxdepth" => {
            if i + 1 >= args.len() {
                return Err("-maxdepth requires an argument".to_string());
            }
            let n: usize = args[i + 1]
                .parse()
                .map_err(|_| format!("-maxdepth: {}: not a number", args[i + 1]))?;
            Ok((Expr::MaxDepth(n), i + 2))
        }
        "-mindepth" => {
            if i + 1 >= args.len() {
                return Err("-mindepth requires an argument".to_string());
            }
            let n: usize = args[i + 1]
                .parse()
                .map_err(|_| format!("-mindepth: {}: not a number", args[i + 1]))?;
            Ok((Expr::MinDepth(n), i + 2))
        }
        "-print" => Ok((Expr::Print, i + 1)),
        _ => Err(format!("unknown expression: {}", arg)),
    }
}

/// Parse expression-only args (for grouped expressions).
fn parse_args_exprs(args: &[String]) -> Result<Option<Expr>, String> {
    let mut exprs = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let (expr, next_i) = parse_single_expr(args, i)?;
        exprs.push(expr);
        i = next_i;
    }

    if exprs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(combine_and(exprs)))
    }
}

/// Extract depth settings from an expression.
fn extract_depth(expr: &Expr) -> Option<(Option<usize>, Option<usize>)> {
    match expr {
        Expr::MaxDepth(n) => Some((Some(*n), None)),
        Expr::MinDepth(n) => Some((None, Some(*n))),
        _ => None,
    }
}

/// Combine a list of expressions with AND.
fn combine_and(mut exprs: Vec<Expr>) -> Expr {
    if exprs.len() == 1 {
        return exprs.remove(0);
    }
    let first = exprs.remove(0);
    exprs
        .into_iter()
        .fold(first, |acc, e| Expr::And(Box::new(acc), Box::new(e)))
}
