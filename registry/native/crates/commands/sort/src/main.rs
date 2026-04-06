use std::cmp::Ordering;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};

#[derive(Debug, Default)]
struct SortOptions {
    reverse: bool,
    numeric: bool,
    key_field: Option<usize>,
    files: Vec<String>,
}

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("sort: {message}");
            return 2;
        }
    };

    let mut lines = match read_input(&options.files) {
        Ok(lines) => lines,
        Err(error) => {
            eprintln!("sort: {error}");
            return 1;
        }
    };

    lines.sort_by(|left, right| compare_lines(left, right, &options));

    let mut stdout = io::stdout().lock();
    for line in &lines {
        if writeln!(stdout, "{line}").is_err() {
            eprintln!("sort: failed to write stdout");
            return 1;
        }
    }

    0
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<SortOptions, String> {
    let mut options = SortOptions::default();
    let mut positional_only = false;
    let mut iter = args.into_iter().peekable();

    while let Some(arg) = iter.next() {
        if positional_only {
            options.files.push(arg);
            continue;
        }

        if arg == "--" {
            positional_only = true;
            continue;
        }

        if arg == "-" || !arg.starts_with('-') {
            options.files.push(arg);
            continue;
        }

        if let Some(value) = arg.strip_prefix("-k") {
            let field = if value.is_empty() {
                iter.next()
                    .ok_or_else(|| "option requires an argument -- 'k'".to_string())?
            } else {
                value.to_string()
            };
            options.key_field = Some(parse_key_field(&field)?);
            continue;
        }

        for flag in arg[1..].chars() {
            match flag {
                'r' => options.reverse = true,
                'n' => options.numeric = true,
                other => {
                    return Err(format!("unsupported option -- '{other}'"));
                }
            }
        }
    }

    Ok(options)
}

fn parse_key_field(value: &str) -> Result<usize, String> {
    let Some(field) = value.split(',').next() else {
        return Err("invalid key field".to_string());
    };
    let parsed = field
        .parse::<usize>()
        .map_err(|_| format!("invalid key field '{value}'"))?;
    if parsed == 0 {
        return Err("key field is 1-based".to_string());
    }
    Ok(parsed)
}

fn read_input(files: &[String]) -> io::Result<Vec<String>> {
    let mut lines = Vec::new();

    if files.is_empty() {
        read_lines_from(io::stdin().lock(), &mut lines)?;
        return Ok(lines);
    }

    for file in files {
        if file == "-" {
            read_lines_from(io::stdin().lock(), &mut lines)?;
            continue;
        }
        let handle = File::open(file)?;
        read_lines_from(BufReader::new(handle), &mut lines)?;
    }

    Ok(lines)
}

fn read_lines_from(reader: impl BufRead, lines: &mut Vec<String>) -> io::Result<()> {
    for line in reader.lines() {
        lines.push(line?);
    }
    Ok(())
}

fn compare_lines(left: &str, right: &str, options: &SortOptions) -> Ordering {
    let mut ordering = if options.numeric {
        compare_numeric(left, right, options.key_field)
    } else {
        compare_text(left, right, options.key_field)
    };

    if ordering == Ordering::Equal {
        ordering = left.cmp(right);
    }

    if options.reverse {
        ordering.reverse()
    } else {
        ordering
    }
}

fn compare_text(left: &str, right: &str, key_field: Option<usize>) -> Ordering {
    extract_key(left, key_field).cmp(extract_key(right, key_field))
}

fn compare_numeric(left: &str, right: &str, key_field: Option<usize>) -> Ordering {
    let left_key = extract_key(left, key_field);
    let right_key = extract_key(right, key_field);
    match (left_key.parse::<f64>(), right_key.parse::<f64>()) {
        (Ok(left_num), Ok(right_num)) => left_num.partial_cmp(&right_num).unwrap_or(Ordering::Equal),
        _ => left_key.cmp(right_key),
    }
}

fn extract_key<'a>(line: &'a str, key_field: Option<usize>) -> &'a str {
    match key_field {
        Some(field) => line.split_whitespace().nth(field.saturating_sub(1)).unwrap_or(""),
        None => line,
    }
}
