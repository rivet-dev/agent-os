//! diff -- compare files line by line using the `similar` crate

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use similar::{ChangeTag, TextDiff};

struct Options {
    unified: bool,
    context_fmt: bool,
    context_lines: usize,
    recursive: bool,
    brief: bool,
    ignore_case: bool,
    ignore_whitespace: bool,
    ignore_blank_lines: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            unified: false,
            context_fmt: false,
            context_lines: 3,
            recursive: false,
            brief: false,
            ignore_case: false,
            ignore_whitespace: false,
            ignore_blank_lines: false,
        }
    }
}

pub fn main(args: Vec<OsString>) -> i32 {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut opts = Options::default();
    let mut files: Vec<String> = Vec::new();
    let mut i = 1;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-u" | "--unified" => opts.unified = true,
            "-c" | "--context" => opts.context_fmt = true,
            "-r" | "-R" | "--recursive" => opts.recursive = true,
            "-q" | "--brief" => opts.brief = true,
            "-i" | "--ignore-case" => opts.ignore_case = true,
            "-w" | "--ignore-all-space" => opts.ignore_whitespace = true,
            "-B" | "--ignore-blank-lines" => opts.ignore_blank_lines = true,
            s if s.starts_with("-U") => {
                if let Ok(n) = s[2..].parse::<usize>() {
                    opts.unified = true;
                    opts.context_lines = n;
                }
            }
            s if s.starts_with("-C") => {
                if let Ok(n) = s[2..].parse::<usize>() {
                    opts.context_fmt = true;
                    opts.context_lines = n;
                }
            }
            _ if !arg.starts_with('-') || arg == "-" => {
                files.push(arg.clone());
            }
            _ => {
                eprintln!("diff: unknown option: {}", arg);
                return 2;
            }
        }
        i += 1;
    }

    if files.len() != 2 {
        eprintln!("diff: requires exactly two file or directory arguments");
        return 2;
    }

    let path_a = Path::new(&files[0]);
    let path_b = Path::new(&files[1]);

    match diff_paths(path_a, path_b, &opts) {
        Ok(has_diff) => {
            if has_diff {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("diff: {}", e);
            2
        }
    }
}

fn diff_paths(path_a: &Path, path_b: &Path, opts: &Options) -> Result<bool, String> {
    let a_is_dir = path_a.is_dir();
    let b_is_dir = path_b.is_dir();

    if a_is_dir && b_is_dir {
        if opts.recursive {
            diff_dirs(path_a, path_b, opts)
        } else {
            Err(format!("{} is a directory", path_a.display()))
        }
    } else if a_is_dir || b_is_dir {
        if a_is_dir {
            let name = path_b.file_name().ok_or("invalid filename")?;
            diff_files(&path_a.join(name), path_b, opts)
        } else {
            let name = path_a.file_name().ok_or("invalid filename")?;
            diff_files(path_a, &path_b.join(name), opts)
        }
    } else {
        diff_files(path_a, path_b, opts)
    }
}

fn diff_dirs(dir_a: &Path, dir_b: &Path, opts: &Options) -> Result<bool, String> {
    let mut entries_a = list_dir(dir_a)?;
    let mut entries_b = list_dir(dir_b)?;
    entries_a.sort();
    entries_b.sort();

    let mut all_names: Vec<String> = Vec::new();
    for name in &entries_a {
        all_names.push(name.clone());
    }
    for name in &entries_b {
        if !entries_a.contains(name) {
            all_names.push(name.clone());
        }
    }
    all_names.sort();

    let mut has_diff = false;

    for name in &all_names {
        let pa = dir_a.join(name);
        let pb = dir_b.join(name);
        let a_exists = entries_a.contains(name);
        let b_exists = entries_b.contains(name);

        if a_exists && !b_exists {
            println!("Only in {}: {}", dir_a.display(), name);
            has_diff = true;
        } else if !a_exists && b_exists {
            println!("Only in {}: {}", dir_b.display(), name);
            has_diff = true;
        } else {
            match diff_paths(&pa, &pb, opts) {
                Ok(d) => {
                    if d {
                        has_diff = true;
                    }
                }
                Err(e) => {
                    eprintln!("diff: {}", e);
                    has_diff = true;
                }
            }
        }
    }

    Ok(has_diff)
}

fn list_dir(dir: &Path) -> Result<Vec<String>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {}", dir.display(), e))?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {}", dir.display(), e))?;
        if let Some(name) = entry.file_name().to_str() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

fn preprocess(text: &str, opts: &Options) -> String {
    let mut s = text.to_string();
    if opts.ignore_blank_lines {
        let trailing = s.ends_with('\n');
        s = s
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if trailing {
            s.push('\n');
        }
    }
    if opts.ignore_case {
        s = s.to_lowercase();
    }
    if opts.ignore_whitespace {
        let trailing = s.ends_with('\n');
        s = s
            .lines()
            .map(|line| {
                let mut result = String::new();
                let mut in_ws = false;
                for ch in line.chars() {
                    if ch.is_whitespace() {
                        if !in_ws {
                            result.push(' ');
                            in_ws = true;
                        }
                    } else {
                        result.push(ch);
                        in_ws = false;
                    }
                }
                let trimmed = result.trim();
                trimmed.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if trailing {
            s.push('\n');
        }
    }
    s
}

fn diff_files(path_a: &Path, path_b: &Path, opts: &Options) -> Result<bool, String> {
    let text_a = read_file(path_a)?;
    let text_b = read_file(path_b)?;

    // Check for differences (possibly with preprocessing)
    let needs_pp = opts.ignore_case || opts.ignore_whitespace || opts.ignore_blank_lines;
    let has_changes = if needs_pp {
        preprocess(&text_a, opts) != preprocess(&text_b, opts)
    } else {
        text_a != text_b
    };

    if !has_changes {
        return Ok(false);
    }

    if opts.brief {
        println!("Files {} and {} differ", path_a.display(), path_b.display());
        return Ok(true);
    }

    // Diff the original text for output (all output logic inline to avoid lifetime issues)
    let diff = TextDiff::from_lines(&text_a, &text_b);
    let label_a = format!("{}", path_a.display());
    let label_b = format!("{}", path_b.display());
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if opts.unified {
        let _ = writeln!(out, "--- {}", label_a);
        let _ = writeln!(out, "+++ {}", label_b);
        for hunk in diff
            .unified_diff()
            .context_radius(opts.context_lines)
            .iter_hunks()
        {
            let _ = write!(out, "{}", hunk);
        }
    } else if opts.context_fmt {
        let _ = writeln!(out, "*** {}", label_a);
        let _ = writeln!(out, "--- {}", label_b);
        for hunk in diff
            .unified_diff()
            .context_radius(opts.context_lines)
            .iter_hunks()
        {
            let mut old_lines: Vec<(ChangeTag, String)> = Vec::new();
            let mut new_lines: Vec<(ChangeTag, String)> = Vec::new();
            let mut old_start = 0usize;
            let mut new_start = 0usize;
            let mut first = true;
            for change in hunk.iter_changes() {
                if first {
                    old_start = change.old_index().unwrap_or(0) + 1;
                    new_start = change.new_index().unwrap_or(0) + 1;
                    first = false;
                }
                match change.tag() {
                    ChangeTag::Equal => {
                        old_lines.push((ChangeTag::Equal, change.value().to_string()));
                        new_lines.push((ChangeTag::Equal, change.value().to_string()));
                    }
                    ChangeTag::Delete => {
                        old_lines.push((ChangeTag::Delete, change.value().to_string()));
                    }
                    ChangeTag::Insert => {
                        new_lines.push((ChangeTag::Insert, change.value().to_string()));
                    }
                }
            }
            let old_end = old_start + old_lines.len().saturating_sub(1);
            let new_end = new_start + new_lines.len().saturating_sub(1);
            let _ = writeln!(out, "***************");
            let _ = writeln!(out, "*** {},{} ****", old_start, old_end);
            for (tag, line) in &old_lines {
                let prefix = match tag {
                    ChangeTag::Delete => "- ",
                    ChangeTag::Equal => "  ",
                    _ => continue,
                };
                let _ = write!(out, "{}{}", prefix, line);
                if !line.ends_with('\n') {
                    let _ = writeln!(out);
                }
            }
            let _ = writeln!(out, "--- {},{} ----", new_start, new_end);
            for (tag, line) in &new_lines {
                let prefix = match tag {
                    ChangeTag::Insert => "+ ",
                    ChangeTag::Equal => "  ",
                    _ => continue,
                };
                let _ = write!(out, "{}{}", prefix, line);
                if !line.ends_with('\n') {
                    let _ = writeln!(out);
                }
            }
        }
    } else {
        // Normal diff format
        let old_text: String = diff.old_slices().concat();
        let new_text: String = diff.new_slices().concat();
        let old_lines: Vec<&str> = old_text.lines().collect();
        let new_lines: Vec<&str> = new_text.lines().collect();

        for op in diff.ops() {
            match op {
                similar::DiffOp::Equal { .. } => {}
                similar::DiffOp::Delete {
                    old_index,
                    old_len,
                    new_index,
                } => {
                    let _ = writeln!(
                        out,
                        "{}d{}",
                        format_range(*old_index + 1, *old_len),
                        new_index
                    );
                    for i in *old_index..*old_index + old_len {
                        if i < old_lines.len() {
                            let _ = writeln!(out, "< {}", old_lines[i]);
                        }
                    }
                }
                similar::DiffOp::Insert {
                    old_index,
                    new_index,
                    new_len,
                } => {
                    let _ = writeln!(
                        out,
                        "{}a{}",
                        old_index,
                        format_range(*new_index + 1, *new_len)
                    );
                    for i in *new_index..*new_index + new_len {
                        if i < new_lines.len() {
                            let _ = writeln!(out, "> {}", new_lines[i]);
                        }
                    }
                }
                similar::DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let _ = writeln!(
                        out,
                        "{}c{}",
                        format_range(*old_index + 1, *old_len),
                        format_range(*new_index + 1, *new_len)
                    );
                    for i in *old_index..*old_index + old_len {
                        if i < old_lines.len() {
                            let _ = writeln!(out, "< {}", old_lines[i]);
                        }
                    }
                    let _ = writeln!(out, "---");
                    for i in *new_index..*new_index + new_len {
                        if i < new_lines.len() {
                            let _ = writeln!(out, "> {}", new_lines[i]);
                        }
                    }
                }
            }
        }
    }

    Ok(true)
}

fn read_file(path: &Path) -> Result<String, String> {
    if path.to_str() == Some("-") {
        use std::io::Read;
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("stdin: {}", e))?;
        Ok(buf)
    } else {
        fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))
    }
}

fn format_range(start: usize, len: usize) -> String {
    if len == 1 {
        format!("{}", start)
    } else {
        format!("{},{}", start, start + len - 1)
    }
}
