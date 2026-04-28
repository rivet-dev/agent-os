//! column -- format stdin/files into columns

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};

pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut table_mode = false;
    let mut separator = String::new(); // empty = default whitespace splitting
    let mut filenames: Vec<String> = Vec::new();

    let mut i = 0;
    while i < str_args.len() {
        match str_args[i].as_str() {
            "-t" => table_mode = true,
            "-s" => {
                i += 1;
                if i >= str_args.len() {
                    eprintln!("column: option '-s' requires an argument");
                    return 1;
                }
                separator = str_args[i].clone();
            }
            s if s.starts_with("-s") && s.len() > 2 => {
                separator = s[2..].to_string();
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("column: unknown option '{}'", s);
                return 1;
            }
            _ => filenames.push(str_args[i].clone()),
        }
        i += 1;
    }

    // Collect all input lines
    let mut lines: Vec<String> = Vec::new();
    if filenames.is_empty() {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => lines.push(l),
                Err(e) => {
                    eprintln!("column: {}", e);
                    return 1;
                }
            }
        }
    } else {
        for filename in &filenames {
            match std::fs::File::open(filename) {
                Ok(f) => {
                    for line in BufReader::new(f).lines() {
                        match line {
                            Ok(l) => lines.push(l),
                            Err(e) => {
                                eprintln!("column: {}: {}", filename, e);
                                return 1;
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("column: {}: {}", filename, e);
                    return 1;
                }
            }
        }
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if table_mode {
        format_table(&lines, &separator, &mut out);
    } else {
        format_columns(&lines, &mut out);
    }

    0
}

/// Table mode (-t): split each line into fields and pad to column widths.
fn format_table<W: Write>(lines: &[String], separator: &str, out: &mut W) {
    // Split lines into rows of fields
    let rows: Vec<Vec<&str>> = lines
        .iter()
        .map(|line| {
            if separator.is_empty() {
                line.split_whitespace().collect()
            } else {
                line.split(&separator[..]).collect()
            }
        })
        .collect();

    if rows.is_empty() {
        return;
    }

    // Find max width for each column
    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut col_widths = vec![0usize; max_cols];
    for row in &rows {
        for (j, field) in row.iter().enumerate() {
            let width = field.chars().count();
            if width > col_widths[j] {
                col_widths[j] = width;
            }
        }
    }

    // Print padded rows
    for row in &rows {
        for (j, field) in row.iter().enumerate() {
            if j > 0 {
                let _ = write!(out, "  ");
            }
            if j + 1 < row.len() {
                // Pad all columns except the last
                let width = field.chars().count();
                let _ = write!(out, "{}", field);
                for _ in width..col_widths[j] {
                    let _ = write!(out, " ");
                }
            } else {
                let _ = write!(out, "{}", field);
            }
        }
        let _ = writeln!(out);
    }
}

/// Default mode: fill columns across the terminal width (simplified: 80 chars).
fn format_columns<W: Write>(lines: &[String], out: &mut W) {
    // Filter out empty lines
    let entries: Vec<&str> = lines
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .collect();
    if entries.is_empty() {
        return;
    }

    let term_width: usize = 80;
    let max_entry_width = entries.iter().map(|e| e.chars().count()).max().unwrap_or(0);
    let col_width = max_entry_width + 2; // 2-char padding

    if col_width == 0 || col_width > term_width {
        // One per line
        for entry in &entries {
            let _ = writeln!(out, "{}", entry);
        }
        return;
    }

    let num_cols = term_width / col_width;
    let num_cols = if num_cols == 0 { 1 } else { num_cols };

    for (i, entry) in entries.iter().enumerate() {
        let is_last_in_row = (i + 1) % num_cols == 0 || i + 1 == entries.len();
        if is_last_in_row {
            let _ = writeln!(out, "{}", entry);
        } else {
            let width = entry.chars().count();
            let _ = write!(out, "{}", entry);
            for _ in width..col_width {
                let _ = write!(out, " ");
            }
        }
    }
}
