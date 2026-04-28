//! rg (ripgrep) implementation using the regex crate (ripgrep's pure Rust regex engine).
//!
//! Provides ripgrep-compatible search. Uses the same regex engine as ripgrep.
//! POSIX grep/egrep/fgrep remain in lib.rs for BRE/ERE/fixed string compatibility.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};

/// Entry point for rg command.
pub fn rg(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    match run(&str_args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("rg: {}", msg);
            2
        }
    }
}

struct Options {
    patterns: Vec<String>,
    paths: Vec<String>,
    files_mode: bool,
    ignore_case: bool,
    smart_case: bool,
    invert_match: bool,
    count_only: bool,
    files_with_matches: bool,
    files_without_matches: bool,
    line_numbers: Option<bool>,
    word_regexp: bool,
    line_regexp: bool,
    fixed_strings: bool,
    max_count: Option<usize>,
    quiet: bool,
    only_matching: bool,
    after_context: usize,
    before_context: usize,
    show_filename: Option<bool>,
    hidden: bool,
    max_depth: Option<usize>,
    sort_modified: bool,
    glob_patterns: Vec<String>,
    type_include: Vec<String>,
    type_exclude: Vec<String>,
}

impl Options {
    fn new() -> Self {
        Self {
            patterns: Vec::new(),
            paths: Vec::new(),
            files_mode: false,
            ignore_case: false,
            smart_case: true,
            invert_match: false,
            count_only: false,
            files_with_matches: false,
            files_without_matches: false,
            line_numbers: None,
            word_regexp: false,
            line_regexp: false,
            fixed_strings: false,
            max_count: None,
            quiet: false,
            only_matching: false,
            after_context: 0,
            before_context: 0,
            show_filename: None,
            hidden: false,
            max_depth: None,
            sort_modified: false,
            glob_patterns: Vec::new(),
            type_include: Vec::new(),
            type_exclude: Vec::new(),
        }
    }

    fn show_line_numbers(&self) -> bool {
        self.line_numbers.unwrap_or(true)
    }

    fn resolve_show_filename(&self, multi: bool) -> bool {
        self.show_filename.unwrap_or(multi)
    }

    fn has_context(&self) -> bool {
        self.before_context > 0 || self.after_context > 0
    }
}

fn run(args: &[String]) -> Result<i32, String> {
    if args.len() == 1 && (args[0] == "--version" || args[0] == "-V") {
        println!("ripgrep 14.1.0 (Agent OS)");
        return Ok(0);
    }

    let opts = parse_args(args)?;

    if opts.files_mode {
        let paths = if opts.paths.is_empty() {
            vec![".".to_string()]
        } else {
            opts.paths.clone()
        };

        let files = collect_files_from_paths(&paths, &opts);
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for path in files {
            let _ = writeln!(out, "{}", path.to_string_lossy());
        }
        return Ok(0);
    }

    if opts.patterns.is_empty() {
        return Err("no pattern provided".to_string());
    }

    let regex = build_regex(&opts)?;

    if opts.paths.is_empty() {
        // No paths: read from stdin
        let stdin = io::stdin();
        let result = search_stream(stdin.lock(), &regex, &opts);
        if opts.quiet {
            return Ok(if result.matches > 0 { 0 } else { 1 });
        }
        print_file_result(None, &result, &opts);
        return Ok(if result.matches > 0 { 0 } else { 1 });
    }

    let files = collect_files_from_paths(&opts.paths, &opts);
    let multi = files.len() > 1;
    let show_fn = opts.resolve_show_filename(multi);
    let mut any_match = false;

    for path in &files {
        match std::fs::File::open(path) {
            Ok(f) => {
                let reader = io::BufReader::new(f);
                let result = search_stream(reader, &regex, &opts);
                if result.matches > 0 {
                    any_match = true;
                }
                if opts.quiet && any_match {
                    return Ok(0);
                }
                if !opts.quiet {
                    let fname = if show_fn {
                        Some(path.to_string_lossy().to_string())
                    } else {
                        None
                    };
                    print_file_result(fname.as_deref(), &result, &opts);
                }
            }
            Err(e) => {
                eprintln!("rg: {}: {}", path.display(), e);
            }
        }
    }

    Ok(if any_match { 0 } else { 1 })
}

// --- Argument parsing ---

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut opts = Options::new();
    let mut i = 0;
    let mut explicit_pattern = false;

    while i < args.len() {
        let arg = &args[i];

        if arg == "--" {
            i += 1;
            break;
        }

        // Long options
        if arg.starts_with("--") {
            match arg.as_str() {
                "--ignore-case" => opts.ignore_case = true,
                "--case-sensitive" => {
                    opts.ignore_case = false;
                    opts.smart_case = false;
                }
                "--smart-case" => opts.smart_case = true,
                "--invert-match" => opts.invert_match = true,
                "--count" => opts.count_only = true,
                "--files" => opts.files_mode = true,
                "--files-with-matches" => opts.files_with_matches = true,
                "--files-without-match" => opts.files_without_matches = true,
                "--line-number" => opts.line_numbers = Some(true),
                "--no-line-number" => opts.line_numbers = Some(false),
                "--word-regexp" => opts.word_regexp = true,
                "--line-regexp" => opts.line_regexp = true,
                "--fixed-strings" => opts.fixed_strings = true,
                "--quiet" | "--silent" => opts.quiet = true,
                "--only-matching" => opts.only_matching = true,
                "--hidden" | "--no-ignore" => opts.hidden = true,
                "--follow" | "--no-ignore-vcs" | "--no-ignore-parent" => {}
                "--with-filename" => opts.show_filename = Some(true),
                "--no-filename" => opts.show_filename = Some(false),
                "--no-heading" | "--heading" => {} // no-op (we always use inline format)
                "--color=auto" | "--color=always" | "--color=never" => {} // no-op in WASI
                "--no-color" => {}
                _ if arg.starts_with("--color=") => {}
                _ if arg.starts_with("--max-depth=") => {
                    opts.max_depth = Some(
                        arg[12..]
                            .parse()
                            .map_err(|_| format!("invalid number: '{}'", &arg[12..]))?,
                    );
                }
                _ if arg.starts_with("--sort=") => match &arg[7..] {
                    "modified" => opts.sort_modified = true,
                    value => return Err(format!("unsupported sort: '{}'", value)),
                },
                _ if arg.starts_with("--threads=") => {}
                _ if arg.starts_with("--regexp=") => {
                    opts.patterns.push(arg[9..].to_string());
                    explicit_pattern = true;
                }
                _ if arg.starts_with("--max-count=") => {
                    opts.max_count = Some(
                        arg[12..]
                            .parse()
                            .map_err(|_| format!("invalid number: '{}'", &arg[12..]))?,
                    );
                }
                _ if arg.starts_with("--after-context=") => {
                    opts.after_context = arg[16..]
                        .parse()
                        .map_err(|_| format!("invalid number: '{}'", &arg[16..]))?;
                }
                _ if arg.starts_with("--before-context=") => {
                    opts.before_context = arg[17..]
                        .parse()
                        .map_err(|_| format!("invalid number: '{}'", &arg[17..]))?;
                }
                _ if arg.starts_with("--context=") => {
                    let n: usize = arg[10..]
                        .parse()
                        .map_err(|_| format!("invalid number: '{}'", &arg[10..]))?;
                    opts.before_context = n;
                    opts.after_context = n;
                }
                _ if arg.starts_with("--glob=") => {
                    opts.glob_patterns.push(arg[7..].to_string());
                }
                _ if arg.starts_with("--type=") => {
                    opts.type_include.push(arg[7..].to_string());
                }
                _ if arg.starts_with("--type-not=") => {
                    opts.type_exclude.push(arg[11..].to_string());
                }
                "--regexp" | "--max-count" | "--after-context" | "--before-context"
                | "--context" | "--glob" | "--type" | "--type-not" | "--file" | "--color"
                | "--max-depth" | "--threads" => {
                    i += 1;
                    if i >= args.len() {
                        return Err(format!("{} requires an argument", arg));
                    }
                    match arg.as_str() {
                        "--regexp" => {
                            opts.patterns.push(args[i].clone());
                            explicit_pattern = true;
                        }
                        "--max-count" => {
                            opts.max_count = Some(
                                args[i]
                                    .parse()
                                    .map_err(|_| format!("invalid number: '{}'", args[i]))?,
                            );
                        }
                        "--after-context" => {
                            opts.after_context = args[i]
                                .parse()
                                .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                        }
                        "--before-context" => {
                            opts.before_context = args[i]
                                .parse()
                                .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                        }
                        "--context" => {
                            let n: usize = args[i]
                                .parse()
                                .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                            opts.before_context = n;
                            opts.after_context = n;
                        }
                        "--glob" => opts.glob_patterns.push(args[i].clone()),
                        "--type" => opts.type_include.push(args[i].clone()),
                        "--type-not" => opts.type_exclude.push(args[i].clone()),
                        "--file" => {
                            let content = std::fs::read_to_string(&args[i])
                                .map_err(|e| format!("{}: {}", args[i], e))?;
                            for line in content.lines() {
                                if !line.is_empty() {
                                    opts.patterns.push(line.to_string());
                                }
                            }
                            explicit_pattern = true;
                        }
                        "--color" => {} // no-op
                        "--max-depth" => {
                            opts.max_depth = Some(
                                args[i]
                                    .parse()
                                    .map_err(|_| format!("invalid number: '{}'", args[i]))?,
                            );
                        }
                        "--threads" => {} // no-op
                        _ => unreachable!(),
                    }
                }
                _ => return Err(format!("unrecognized option '{}'", arg)),
            }
            i += 1;
            continue;
        }

        // Short options
        if arg.starts_with('-') && arg.len() > 1 {
            let chars: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                match chars[j] {
                    'i' => opts.ignore_case = true,
                    's' => {
                        opts.ignore_case = false;
                        opts.smart_case = false;
                    }
                    'S' => opts.smart_case = true,
                    'v' => opts.invert_match = true,
                    'c' => opts.count_only = true,
                    'l' => opts.files_with_matches = true,
                    'n' => opts.line_numbers = Some(true),
                    'N' => opts.line_numbers = Some(false),
                    'w' => opts.word_regexp = true,
                    'x' => opts.line_regexp = true,
                    'F' => opts.fixed_strings = true,
                    'q' => opts.quiet = true,
                    'o' => opts.only_matching = true,
                    'H' => opts.show_filename = Some(true),
                    '.' => opts.hidden = true,
                    'e' => {
                        let rest: String = chars[j + 1..].iter().collect();
                        if !rest.is_empty() {
                            opts.patterns.push(rest);
                        } else {
                            i += 1;
                            if i >= args.len() {
                                return Err("option requires an argument -- 'e'".to_string());
                            }
                            opts.patterns.push(args[i].clone());
                        }
                        explicit_pattern = true;
                        j = chars.len();
                        continue;
                    }
                    'f' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'f'".to_string());
                        }
                        let content = std::fs::read_to_string(&args[i])
                            .map_err(|e| format!("{}: {}", args[i], e))?;
                        for line in content.lines() {
                            if !line.is_empty() {
                                opts.patterns.push(line.to_string());
                            }
                        }
                        explicit_pattern = true;
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
                                .map_err(|_| format!("invalid number: '{}'", args[i]))?,
                        );
                        j = chars.len();
                        continue;
                    }
                    'j' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'j'".to_string());
                        }
                        j = chars.len();
                        continue;
                    }
                    'A' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'A'".to_string());
                        }
                        opts.after_context = args[i]
                            .parse()
                            .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                        j = chars.len();
                        continue;
                    }
                    'B' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'B'".to_string());
                        }
                        opts.before_context = args[i]
                            .parse()
                            .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                        j = chars.len();
                        continue;
                    }
                    'C' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'C'".to_string());
                        }
                        let n: usize = args[i]
                            .parse()
                            .map_err(|_| format!("invalid number: '{}'", args[i]))?;
                        opts.before_context = n;
                        opts.after_context = n;
                        j = chars.len();
                        continue;
                    }
                    'g' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'g'".to_string());
                        }
                        opts.glob_patterns.push(args[i].clone());
                        j = chars.len();
                        continue;
                    }
                    't' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 't'".to_string());
                        }
                        opts.type_include.push(args[i].clone());
                        j = chars.len();
                        continue;
                    }
                    'T' => {
                        i += 1;
                        if i >= args.len() {
                            return Err("option requires an argument -- 'T'".to_string());
                        }
                        opts.type_exclude.push(args[i].clone());
                        j = chars.len();
                        continue;
                    }
                    _ => return Err(format!("invalid option -- '{}'", chars[j])),
                }
                j += 1;
            }
            i += 1;
            continue;
        }

        // Positional argument
        if opts.files_mode {
            opts.paths.push(arg.clone());
        } else if !explicit_pattern && opts.patterns.is_empty() {
            opts.patterns.push(arg.clone());
            explicit_pattern = true;
        } else {
            opts.paths.push(arg.clone());
        }
        i += 1;
    }

    // Remaining args after --
    while i < args.len() {
        if opts.files_mode {
            opts.paths.push(args[i].clone());
        } else if !explicit_pattern && opts.patterns.is_empty() {
            opts.patterns.push(args[i].clone());
            explicit_pattern = true;
        } else {
            opts.paths.push(args[i].clone());
        }
        i += 1;
    }

    Ok(opts)
}

// --- Pattern building ---

fn build_regex(opts: &Options) -> Result<Regex, String> {
    let combined = if opts.patterns.len() == 1 {
        prepare_pattern(&opts.patterns[0], opts)
    } else {
        let parts: Vec<String> = opts
            .patterns
            .iter()
            .map(|p| format!("(?:{})", prepare_pattern(p, opts)))
            .collect();
        parts.join("|")
    };

    let case_insensitive = if opts.ignore_case {
        true
    } else if opts.smart_case {
        // Smart case: insensitive unless pattern has uppercase
        !combined.chars().any(|c| c.is_uppercase())
    } else {
        false
    };

    RegexBuilder::new(&combined)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|e| format!("regex error: {}", e))
}

fn prepare_pattern(pattern: &str, opts: &Options) -> String {
    let base = if opts.fixed_strings {
        regex::escape(pattern)
    } else {
        pattern.to_string()
    };

    if opts.word_regexp {
        format!(r"\b(?:{})\b", base)
    } else if opts.line_regexp {
        format!("^(?:{})$", base)
    } else {
        base
    }
}

// --- File collection ---

fn collect_files_from_paths(paths: &[String], opts: &Options) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path_str in paths {
        let path = Path::new(path_str);
        if path.is_dir() {
            walk_dir(path, path, opts, &mut files, 0);
        } else if path.is_file() && should_include(path, path, false, opts) {
            files.push(path.to_path_buf());
        }
    }
    if opts.sort_modified {
        files.sort_by_key(|path| {
            std::fs::metadata(path)
                .and_then(|meta| meta.modified())
                .ok()
        });
    } else {
        files.sort();
    }
    files
}

fn walk_dir(root: &Path, dir: &Path, opts: &Options, out: &mut Vec<PathBuf>, depth: usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("rg: {}: {}", dir.display(), e);
            return;
        }
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let is_dir = path.is_dir();

        // Skip hidden files/dirs unless --hidden
        if !opts.hidden && name_str.starts_with('.') {
            continue;
        }

        if !should_include(root, &path, is_dir, opts) {
            continue;
        }

        if is_dir {
            if opts.max_depth.map(|max| depth < max).unwrap_or(true) {
                walk_dir(root, &path, opts, out, depth + 1);
            }
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn should_include(root: &Path, path: &Path, is_dir: bool, opts: &Options) -> bool {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();
    let relative_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    // Type include filters
    if !opts.type_include.is_empty() {
        let included = opts.type_include.iter().any(|t| {
            type_extensions(t)
                .map(|exts| exts.iter().any(|e| ext == *e))
                .unwrap_or(false)
        });
        if !included {
            return false;
        }
    }

    // Type exclude filters
    if !opts.type_exclude.is_empty() {
        let excluded = opts.type_exclude.iter().any(|t| {
            type_extensions(t)
                .map(|exts| exts.iter().any(|e| ext == *e))
                .unwrap_or(false)
        });
        if excluded {
            return false;
        }
    }

    // Glob filters
    if !opts.glob_patterns.is_empty() {
        for pattern in &opts.glob_patterns {
            let (negated, pat) = if let Some(rest) = pattern.strip_prefix('!') {
                (true, rest)
            } else {
                (false, pattern.as_str())
            };
            let matches = glob_matches(pat, &relative_path, &file_name, is_dir);
            if negated && matches {
                return false;
            }
            if !negated && !matches {
                return false;
            }
        }
    }

    true
}

fn type_extensions(type_name: &str) -> Option<&'static [&'static str]> {
    match type_name {
        "rust" | "rs" => Some(&["rs"]),
        "py" | "python" => Some(&["py", "pyi"]),
        "js" | "javascript" => Some(&["js", "jsx", "mjs"]),
        "ts" | "typescript" => Some(&["ts", "tsx", "mts"]),
        "c" => Some(&["c", "h"]),
        "cpp" | "c++" => Some(&["cpp", "cxx", "cc", "hpp", "hxx", "h"]),
        "java" => Some(&["java"]),
        "go" => Some(&["go"]),
        "html" => Some(&["html", "htm"]),
        "css" => Some(&["css"]),
        "json" => Some(&["json"]),
        "yaml" | "yml" => Some(&["yml", "yaml"]),
        "toml" => Some(&["toml"]),
        "md" | "markdown" => Some(&["md", "markdown"]),
        "txt" | "text" => Some(&["txt"]),
        "sh" | "shell" | "bash" => Some(&["sh", "bash"]),
        "xml" => Some(&["xml"]),
        "sql" => Some(&["sql"]),
        "lua" => Some(&["lua"]),
        "ruby" | "rb" => Some(&["rb"]),
        "php" => Some(&["php"]),
        "swift" => Some(&["swift"]),
        "kotlin" | "kt" => Some(&["kt", "kts"]),
        _ => None,
    }
}

fn glob_matches(pattern: &str, relative_path: &str, file_name: &str, is_dir: bool) -> bool {
    let normalized = pattern.trim_end_matches('/');
    if pattern.ends_with('/') {
        return is_dir
            && relative_path
                .split('/')
                .any(|segment| segment == normalized || segment == file_name);
    }

    let target = if pattern.contains('/') {
        relative_path
    } else {
        file_name
    };

    let mut regex_pattern = String::from("^");
    let chars: Vec<char> = normalized.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    regex_pattern.push_str(".*");
                    i += 2;
                } else {
                    regex_pattern.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                regex_pattern.push_str("[^/]");
                i += 1;
            }
            '{' => {
                if let Some(end) = chars[i + 1..].iter().position(|c| *c == '}') {
                    let group: String = chars[i + 1..i + 1 + end].iter().collect();
                    regex_pattern.push('(');
                    regex_pattern.push_str(
                        &group
                            .split(',')
                            .map(regex::escape)
                            .collect::<Vec<_>>()
                            .join("|"),
                    );
                    regex_pattern.push(')');
                    i += end + 2;
                } else {
                    regex_pattern.push_str("\\{");
                    i += 1;
                }
            }
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '\\' => {
                regex_pattern.push('\\');
                regex_pattern.push(chars[i]);
                i += 1;
            }
            other => {
                regex_pattern.push(other);
                i += 1;
            }
        }
    }
    regex_pattern.push('$');

    Regex::new(&regex_pattern)
        .map(|regex| regex.is_match(target))
        .unwrap_or(false)
}

// --- Search ---

struct FileResult {
    matches: usize,
    lines: Vec<ResultLine>,
    is_binary: bool,
}

enum ResultLine {
    Match(usize, String),
    Context(usize, String),
    Separator,
}

fn search_stream<R: BufRead>(reader: R, regex: &Regex, opts: &Options) -> FileResult {
    let mut result = FileResult {
        matches: 0,
        lines: Vec::new(),
        is_binary: false,
    };

    let collect_lines =
        !opts.quiet && !opts.files_with_matches && !opts.files_without_matches && !opts.count_only;

    let mut before_buf: VecDeque<(usize, String)> = VecDeque::new();
    let mut after_remaining: usize = 0;
    let mut last_printed: usize = 0;

    for (idx, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(_) => break,
        };
        let lineno = idx + 1;

        // Binary detection: null bytes in line data
        if line.as_bytes().contains(&0) {
            result.is_binary = true;
            break;
        }

        let is_match = regex.is_match(&line) != opts.invert_match;

        if is_match {
            result.matches += 1;

            if opts.quiet || opts.files_with_matches {
                break;
            }

            if collect_lines {
                // Separator for non-contiguous match groups
                if opts.has_context() && last_printed > 0 {
                    let first_before = before_buf.front().map(|(n, _)| *n).unwrap_or(lineno);
                    if first_before > last_printed + 1 {
                        result.lines.push(ResultLine::Separator);
                    }
                }

                // Flush before-context buffer
                for (bno, btext) in before_buf.drain(..) {
                    if bno > last_printed {
                        result.lines.push(ResultLine::Context(bno, btext));
                        last_printed = bno;
                    }
                }

                // Emit match
                if opts.only_matching && !opts.invert_match {
                    for mat in regex.find_iter(&line) {
                        result
                            .lines
                            .push(ResultLine::Match(lineno, mat.as_str().to_string()));
                    }
                } else {
                    result.lines.push(ResultLine::Match(lineno, line));
                }
                last_printed = lineno;
                after_remaining = opts.after_context;
            }

            if let Some(max) = opts.max_count {
                if result.matches >= max {
                    break;
                }
            }
        } else if collect_lines {
            if after_remaining > 0 {
                result.lines.push(ResultLine::Context(lineno, line));
                last_printed = lineno;
                after_remaining -= 1;
            } else if opts.before_context > 0 {
                before_buf.push_back((lineno, line));
                if before_buf.len() > opts.before_context {
                    before_buf.pop_front();
                }
            }
        }
    }

    result
}

// --- Output ---

fn print_file_result(filename: Option<&str>, result: &FileResult, opts: &Options) {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if result.is_binary {
        if result.matches > 0 {
            if let Some(name) = filename {
                let _ = writeln!(out, "Binary file {} matches.", name);
            }
        }
        return;
    }

    if opts.files_with_matches {
        if result.matches > 0 {
            let name = filename.unwrap_or("(standard input)");
            let _ = writeln!(out, "{}", name);
        }
        return;
    }

    if opts.files_without_matches {
        if result.matches == 0 {
            let name = filename.unwrap_or("(standard input)");
            let _ = writeln!(out, "{}", name);
        }
        return;
    }

    if opts.count_only {
        if let Some(name) = filename {
            let _ = writeln!(out, "{}:{}", name, result.matches);
        } else {
            let _ = writeln!(out, "{}", result.matches);
        }
        return;
    }

    for line in &result.lines {
        match line {
            ResultLine::Match(lineno, text) => {
                let mut prefix = String::new();
                if let Some(name) = filename {
                    prefix.push_str(name);
                    prefix.push(':');
                }
                if opts.show_line_numbers() {
                    prefix.push_str(&lineno.to_string());
                    prefix.push(':');
                }
                let _ = writeln!(out, "{}{}", prefix, text);
            }
            ResultLine::Context(lineno, text) => {
                let mut prefix = String::new();
                if let Some(name) = filename {
                    prefix.push_str(name);
                    prefix.push('-');
                }
                if opts.show_line_numbers() {
                    prefix.push_str(&lineno.to_string());
                    prefix.push('-');
                }
                let _ = writeln!(out, "{}{}", prefix, text);
            }
            ResultLine::Separator => {
                let _ = writeln!(out, "--");
            }
        }
    }
}
