//! gzip/gunzip/zcat implementation using flate2.
//!
//! Dispatches on argv[0] basename for standalone binary usage:
//! - "gunzip" -> decompress mode
//! - "zcat" -> decompress + stdout mode
//! - default -> compress mode

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

/// Unified gzip entry point. Dispatches on argv[0]:
/// - "gunzip" -> decompress mode
/// - "zcat" -> decompress + stdout mode
/// - default -> compress mode
pub fn main(args: Vec<OsString>) -> i32 {
    let argv0 = args
        .first()
        .map(|a| a.to_string_lossy().to_string())
        .unwrap_or_default();
    let progname = match argv0.rfind('/') {
        Some(pos) => &argv0[pos + 1..],
        None => &argv0[..],
    };

    // Defaults based on invocation name
    let mut decompress = matches!(progname, "gunzip" | "zcat");
    let mut to_stdout = progname == "zcat";
    let mut keep = false;
    let mut force = false;
    let mut level: u32 = 6; // default compression level
    let mut files: Vec<String> = Vec::new();

    // Parse arguments
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();
    let mut i = 0;
    while i < str_args.len() {
        let arg = &str_args[i];
        if arg == "--" {
            files.extend(str_args[i + 1..].iter().cloned());
            break;
        }
        if arg.starts_with('-') && arg.len() > 1 && !arg.starts_with("--") {
            // Parse combined short flags: -dcfk9
            for ch in arg[1..].chars() {
                match ch {
                    'd' => decompress = true,
                    'c' => to_stdout = true,
                    'k' => keep = true,
                    'f' => force = true,
                    '1'..='9' => level = ch.to_digit(10).unwrap(),
                    'h' => {
                        print_usage(progname);
                        return 0;
                    }
                    _ => {
                        eprintln!("{}: invalid option -- '{}'", progname, ch);
                        return 1;
                    }
                }
            }
        } else if arg == "--decompress" || arg == "--uncompress" {
            decompress = true;
        } else if arg == "--stdout" || arg == "--to-stdout" {
            to_stdout = true;
        } else if arg == "--keep" {
            keep = true;
        } else if arg == "--force" {
            force = true;
        } else if arg == "--fast" {
            level = 1;
        } else if arg == "--best" {
            level = 9;
        } else if arg == "--help" {
            print_usage(progname);
            return 0;
        } else {
            files.push(arg.clone());
        }
        i += 1;
    }

    let compression = Compression::new(level);
    let mut exit_code = 0;

    if files.is_empty() {
        // stdin/stdout mode
        if decompress {
            if let Err(e) = decompress_stream(io::stdin(), io::stdout()) {
                eprintln!("{}: {}", progname, e);
                exit_code = 1;
            }
        } else {
            if let Err(e) = compress_stream(io::stdin(), io::stdout(), compression) {
                eprintln!("{}: {}", progname, e);
                exit_code = 1;
            }
        }
    } else {
        for filename in &files {
            if decompress {
                if let Err(e) = decompress_file(progname, filename, to_stdout, keep, force) {
                    eprintln!("{}: {}", progname, e);
                    exit_code = 1;
                }
            } else {
                if let Err(e) =
                    compress_file(progname, filename, to_stdout, keep, force, compression)
                {
                    eprintln!("{}: {}", progname, e);
                    exit_code = 1;
                }
            }
        }
    }

    exit_code
}

fn compress_stream<R: Read, W: Write>(input: R, output: W, level: Compression) -> io::Result<()> {
    let mut reader = BufReader::new(input);
    let mut encoder = GzEncoder::new(BufWriter::new(output), level);
    io::copy(&mut reader, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

fn decompress_stream<R: Read, W: Write>(input: R, output: W) -> io::Result<()> {
    let mut decoder = GzDecoder::new(BufReader::new(input));
    let mut writer = BufWriter::new(output);
    io::copy(&mut decoder, &mut writer)?;
    writer.flush()?;
    Ok(())
}

fn compress_file(
    progname: &str,
    path: &str,
    to_stdout: bool,
    keep: bool,
    force: bool,
    level: Compression,
) -> io::Result<()> {
    let out_path = format!("{}.gz", path);

    if !to_stdout && !force && std::fs::metadata(&out_path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{}: {} already exists; not overwritten", progname, out_path),
        ));
    }

    let input = File::open(path)?;

    if to_stdout {
        compress_stream(input, io::stdout(), level)?;
    } else {
        let output = File::create(&out_path)?;
        compress_stream(input, output, level)?;
        if !keep {
            std::fs::remove_file(path)?;
        }
    }

    Ok(())
}

fn decompress_file(
    progname: &str,
    path: &str,
    to_stdout: bool,
    keep: bool,
    force: bool,
) -> io::Result<()> {
    // Determine output filename by stripping .gz / .z / .tgz suffix
    let out_path = if path.ends_with(".gz") {
        path[..path.len() - 3].to_string()
    } else if path.ends_with(".tgz") {
        format!("{}.tar", &path[..path.len() - 4])
    } else if path.ends_with(".z") {
        path[..path.len() - 2].to_string()
    } else {
        // No recognized suffix — append .out
        format!("{}.out", path)
    };

    if !to_stdout && !force && std::fs::metadata(&out_path).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{}: {} already exists; not overwritten", progname, out_path),
        ));
    }

    let input = File::open(path)?;

    if to_stdout {
        decompress_stream(input, io::stdout())?;
    } else {
        let output = File::create(&out_path)?;
        decompress_stream(input, output)?;
        if !keep {
            std::fs::remove_file(path)?;
        }
    }

    Ok(())
}

fn print_usage(progname: &str) {
    eprintln!("Usage: {} [-dcfk123456789] [file ...]", progname);
    eprintln!("  -d, --decompress   decompress");
    eprintln!("  -c, --stdout       write to stdout, keep original files");
    eprintln!("  -k, --keep         keep input files");
    eprintln!("  -f, --force        force overwrite of output file");
    eprintln!("  -1..9              compression level (fast..best, default 6)");
    eprintln!("  -h, --help         display this help");
}
