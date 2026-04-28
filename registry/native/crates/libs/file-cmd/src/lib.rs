//! file -- determine file type
//!
//! Magic byte detection using the `infer` crate, plus text/binary heuristic.
//! Supports -b (brief, no filename prefix), -i (output MIME type).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};

pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    let mut brief = false;
    let mut mime = false;
    let mut filenames: Vec<String> = Vec::new();

    let mut i = 0;
    while i < str_args.len() {
        match str_args[i].as_str() {
            "-b" | "--brief" => brief = true,
            "-i" | "--mime-type" | "--mime" => mime = true,
            // Combined short flags
            s if s.starts_with('-') && s.len() > 1 && !s.starts_with("--") => {
                for ch in s[1..].chars() {
                    match ch {
                        'b' => brief = true,
                        'i' => mime = true,
                        _ => {
                            eprintln!("file: unknown option '{}'", s);
                            return 1;
                        }
                    }
                }
            }
            "-" => {
                // Read from stdin
                filenames.push("-".to_string());
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("file: unknown option '{}'", s);
                return 1;
            }
            _ => filenames.push(str_args[i].clone()),
        }
        i += 1;
    }

    if filenames.is_empty() {
        eprintln!("Usage: file [-bi] FILE...");
        return 1;
    }

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut exit_code = 0;

    for filename in &filenames {
        let data = if filename == "-" {
            let mut buf = Vec::new();
            match io::stdin().lock().read_to_end(&mut buf) {
                Ok(_) => buf,
                Err(e) => {
                    eprintln!("file: stdin: {}", e);
                    exit_code = 1;
                    continue;
                }
            }
        } else {
            // Check if path exists and what kind it is
            match fs::metadata(filename) {
                Ok(meta) => {
                    if meta.is_dir() {
                        print_result(
                            &mut out,
                            filename,
                            brief,
                            "directory",
                            if mime { "inode/directory" } else { "" },
                            mime,
                        );
                        continue;
                    }
                    if meta.is_symlink() {
                        print_result(
                            &mut out,
                            filename,
                            brief,
                            "symbolic link",
                            if mime { "inode/symlink" } else { "" },
                            mime,
                        );
                        continue;
                    }
                    if meta.len() == 0 {
                        print_result(
                            &mut out,
                            filename,
                            brief,
                            "empty",
                            if mime { "inode/x-empty" } else { "" },
                            mime,
                        );
                        continue;
                    }
                    // Read file data (up to 8KB for detection)
                    match read_head(filename, 8192) {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!("file: {}: {}", filename, e);
                            exit_code = 1;
                            continue;
                        }
                    }
                }
                Err(e) => {
                    eprintln!("file: {}: {}", filename, e);
                    exit_code = 1;
                    continue;
                }
            }
        };

        let (desc, mime_type) = identify(&data);

        if mime {
            print_result(&mut out, filename, brief, &desc, &mime_type, true);
        } else {
            print_result(&mut out, filename, brief, &desc, &mime_type, false);
        }
    }

    exit_code
}

fn read_head(path: &str, max: usize) -> io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    let mut buf = vec![0u8; max];
    let n = f.read(&mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn print_result<W: Write>(
    out: &mut W,
    filename: &str,
    brief: bool,
    desc: &str,
    mime_type: &str,
    use_mime: bool,
) {
    if use_mime && !mime_type.is_empty() {
        if brief {
            let _ = writeln!(out, "{}", mime_type);
        } else {
            let _ = writeln!(out, "{}: {}", filename, mime_type);
        }
    } else if brief {
        let _ = writeln!(out, "{}", desc);
    } else {
        let _ = writeln!(out, "{}: {}", filename, desc);
    }
}

fn identify(data: &[u8]) -> (String, String) {
    if data.is_empty() {
        return ("empty".to_string(), "inode/x-empty".to_string());
    }

    // Try infer crate for magic byte detection
    if let Some(kind) = infer::get(data) {
        let desc = match kind.mime_type() {
            "image/png" => "PNG image data",
            "image/jpeg" => "JPEG image data",
            "image/gif" => "GIF image data",
            "image/webp" => "WebP image data",
            "image/bmp" => "BMP image data",
            "image/tiff" => "TIFF image data",
            "image/svg+xml" => "SVG image data",
            "image/x-icon" => "ICO image data",
            "application/pdf" => "PDF document",
            "application/zip" => "Zip archive data",
            "application/gzip" => "gzip compressed data",
            "application/x-bzip2" => "bzip2 compressed data",
            "application/x-xz" => "XZ compressed data",
            "application/x-tar" => "POSIX tar archive",
            "application/x-rar-compressed" => "RAR archive data",
            "application/x-7z-compressed" => "7-zip archive data",
            "application/x-executable" | "application/x-elf" => "ELF executable",
            "application/wasm" => "WebAssembly (wasm) binary module",
            "application/x-mach-binary" => "Mach-O binary",
            "audio/mpeg" => "MPEG audio",
            "audio/ogg" => "Ogg audio",
            "audio/flac" => "FLAC audio",
            "audio/wav" | "audio/x-wav" => "RIFF WAVE audio",
            "video/mp4" => "MPEG-4 video",
            "video/webm" => "WebM video",
            "video/x-matroska" => "Matroska video",
            "application/x-sqlite3" => "SQLite 3.x database",
            "font/woff" => "Web Open Font Format",
            "font/woff2" => "Web Open Font Format 2",
            _ => kind.mime_type(),
        };
        return (desc.to_string(), kind.mime_type().to_string());
    }

    // Check for shebang scripts
    if data.len() >= 2 && data[0] == b'#' && data[1] == b'!' {
        let first_line = data
            .iter()
            .take(128)
            .take_while(|&&b| b != b'\n')
            .copied()
            .collect::<Vec<u8>>();
        if let Ok(line) = std::str::from_utf8(&first_line) {
            let interp = line.trim_start_matches("#!");
            let interp = interp.trim();
            // Extract interpreter name
            let name = interp.split_whitespace().next().unwrap_or(interp);
            let basename = match name.rfind('/') {
                Some(pos) => &name[pos + 1..],
                None => name,
            };
            // Handle "env <prog>" pattern
            if basename == "env" {
                let prog = interp.split_whitespace().nth(1).unwrap_or("script");
                return (
                    format!("{} script, ASCII text executable", prog),
                    "text/x-script".to_string(),
                );
            }
            return (
                format!("{} script, ASCII text executable", basename),
                "text/x-script".to_string(),
            );
        }
    }

    // Check for known text patterns
    if is_json(data) {
        return ("JSON text data".to_string(), "application/json".to_string());
    }
    if is_xml(data) {
        return ("XML document".to_string(), "text/xml".to_string());
    }
    if is_html(data) {
        return ("HTML document".to_string(), "text/html".to_string());
    }

    // Text vs binary heuristic
    if is_text(data) {
        return ("ASCII text".to_string(), "text/plain".to_string());
    }

    ("data".to_string(), "application/octet-stream".to_string())
}

fn is_text(data: &[u8]) -> bool {
    // Check first 8KB for non-text bytes
    let check_len = data.len().min(8192);
    let mut null_count = 0;
    for &b in &data[..check_len] {
        if b == 0 {
            null_count += 1;
            if null_count > 0 {
                return false;
            }
        }
        // Allow common control chars: tab, newline, carriage return, form feed, backspace, escape
        if b < 0x08 || (b > 0x0D && b < 0x20 && b != 0x1B) {
            if b != 0x00 {
                // already counted nulls
                return false;
            }
        }
    }
    true
}

fn is_json(data: &[u8]) -> bool {
    // Quick check: starts with { or [, ignoring leading whitespace
    let trimmed = skip_whitespace(data);
    !trimmed.is_empty() && (trimmed[0] == b'{' || trimmed[0] == b'[')
}

fn is_xml(data: &[u8]) -> bool {
    let trimmed = skip_whitespace(data);
    trimmed.starts_with(b"<?xml") || trimmed.starts_with(b"<!DOCTYPE")
}

fn is_html(data: &[u8]) -> bool {
    let trimmed = skip_whitespace(data);
    let lower: Vec<u8> = trimmed
        .iter()
        .take(64)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    lower.starts_with(b"<!doctype html") || lower.starts_with(b"<html")
}

fn skip_whitespace(data: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < data.len()
        && (data[i] == b' ' || data[i] == b'\t' || data[i] == b'\n' || data[i] == b'\r')
    {
        i += 1;
    }
    &data[i..]
}
