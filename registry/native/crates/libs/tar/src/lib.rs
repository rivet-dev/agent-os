//! tar implementation — create, extract, and list tape archives.
//!
//! Supports -c create, -x extract, -t list.
//! Options: -f archive, -z gzip, -v verbose, -C directory, --strip-components=N.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

#[derive(PartialEq)]
enum Mode {
    None,
    Create,
    Extract,
    List,
}

/// Unified tar entry point.
pub fn main(args: Vec<OsString>) -> i32 {
    let str_args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .collect();

    if str_args.is_empty() {
        eprintln!("tar: must specify one of -c, -x, -t");
        return 1;
    }

    let mut mode = Mode::None;
    let mut archive_file: Option<String> = None;
    let mut gzip = false;
    let mut verbose = false;
    let mut directory: Option<String> = None;
    let mut strip_components: usize = 0;
    let mut paths: Vec<String> = Vec::new();

    let mut i = 0;
    let mut first_arg = true;

    while i < str_args.len() {
        let arg = &str_args[i];

        if arg.starts_with("--strip-components=") {
            if let Ok(n) = arg["--strip-components=".len()..].parse() {
                strip_components = n;
            }
            first_arg = false;
        } else if arg == "--strip-components" {
            i += 1;
            if i < str_args.len() {
                strip_components = str_args[i].parse().unwrap_or(0);
            }
            first_arg = false;
        } else if arg == "-C" || arg == "--directory" {
            i += 1;
            if i < str_args.len() {
                directory = Some(str_args[i].clone());
            }
            first_arg = false;
        } else if arg == "--help" {
            print_usage();
            return 0;
        } else if arg.starts_with('-') || first_arg {
            // tar's first argument can omit the leading dash (e.g., `tar czf`)
            let flags = if arg.starts_with('-') {
                &arg[1..]
            } else {
                &arg[..]
            };
            let mut chars = flags.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    'c' => mode = Mode::Create,
                    'x' => mode = Mode::Extract,
                    't' => mode = Mode::List,
                    'z' => gzip = true,
                    'v' => verbose = true,
                    'f' => {
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            archive_file = Some(rest);
                        } else {
                            i += 1;
                            if i < str_args.len() {
                                archive_file = Some(str_args[i].clone());
                            }
                        }
                        break;
                    }
                    'C' => {
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            directory = Some(rest);
                        } else {
                            i += 1;
                            if i < str_args.len() {
                                directory = Some(str_args[i].clone());
                            }
                        }
                        break;
                    }
                    _ => {
                        eprintln!("tar: unknown option: {}", ch);
                        return 1;
                    }
                }
            }
            first_arg = false;
        } else {
            paths.push(arg.clone());
            first_arg = false;
        }

        i += 1;
    }

    // Auto-detect gzip from filename
    if !gzip {
        if let Some(ref f) = archive_file {
            if f.ends_with(".tar.gz") || f.ends_with(".tgz") {
                gzip = true;
            }
        }
    }

    let result = match mode {
        Mode::Create => do_create(
            archive_file.as_deref(),
            gzip,
            verbose,
            directory.as_deref(),
            &paths,
        ),
        Mode::Extract => do_extract(
            archive_file.as_deref(),
            gzip,
            verbose,
            directory.as_deref(),
            strip_components,
        ),
        Mode::List => do_list(archive_file.as_deref(), gzip, verbose),
        Mode::None => {
            eprintln!("tar: must specify one of -c, -x, -t");
            return 1;
        }
    };

    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("tar: {}", e);
            1
        }
    }
}

fn do_create(
    archive_file: Option<&str>,
    gzip: bool,
    verbose: bool,
    directory: Option<&str>,
    paths: &[String],
) -> io::Result<()> {
    if paths.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cowardly refusing to create an empty archive",
        ));
    }

    let bytes = if gzip {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for path in paths {
            append_path(
                &mut builder,
                resolve_disk_path(directory, Path::new(path)),
                Path::new(path),
                verbose,
            )?;
        }
        let encoder = builder.into_inner()?;
        encoder.finish()?
    } else {
        let cursor = io::Cursor::new(Vec::new());
        let mut builder = tar::Builder::new(cursor);
        for path in paths {
            append_path(
                &mut builder,
                resolve_disk_path(directory, Path::new(path)),
                Path::new(path),
                verbose,
            )?;
        }
        let cursor = builder.into_inner()?;
        cursor.into_inner()
    };

    write_archive_bytes(archive_file, &bytes)
}

fn append_path<W: Write>(
    builder: &mut tar::Builder<W>,
    disk_path: PathBuf,
    archive_path: &Path,
    verbose: bool,
) -> io::Result<()> {
    let meta = fs::symlink_metadata(&disk_path)?;

    if meta.is_dir() {
        append_dir(builder, &disk_path, archive_path, verbose)?;
    } else if meta.is_file() {
        if verbose {
            eprintln!("{}", archive_path.display());
        }
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(meta.len());
        header.set_mode(0o644);
        header.set_cksum();
        let mut file = File::open(&disk_path)?;
        builder.append_data(&mut header, archive_path, &mut file)?;
    } else if meta.file_type().is_symlink() {
        if verbose {
            eprintln!("{}", archive_path.display());
        }
        let target = fs::read_link(&disk_path)?;
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        builder.append_link(&mut header, archive_path, &target)?;
    }

    Ok(())
}

fn append_dir<W: Write>(
    builder: &mut tar::Builder<W>,
    disk_dir: &Path,
    archive_dir: &Path,
    verbose: bool,
) -> io::Result<()> {
    if verbose {
        eprintln!("{}/", archive_dir.display());
    }

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, archive_dir, io::empty())?;

    let mut entries: Vec<_> = fs::read_dir(disk_dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let archive_child = archive_dir.join(entry.file_name());
        append_path(builder, entry.path(), &archive_child, verbose)?;
    }

    Ok(())
}

fn do_extract(
    archive_file: Option<&str>,
    gzip: bool,
    verbose: bool,
    directory: Option<&str>,
    strip_components: usize,
) -> io::Result<()> {
    let reader = open_read(archive_file, gzip)?;
    let mut archive = tar::Archive::new(reader);
    let mut known_dirs = HashSet::new();
    if let Some(base) = directory {
        known_dirs.insert(PathBuf::from(base));
    }

    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let orig_path = entry.path()?.into_owned();

        let relative_dest = match strip_path_components(&orig_path, strip_components) {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => continue,
        };
        let dest = resolve_output_path(directory, &relative_dest);

        if verbose {
            eprintln!("{}", orig_path.display());
        }

        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                ensure_relative_dir_exists(directory, &relative_dest, &mut known_dirs)?;
            }
            tar::EntryType::Regular | tar::EntryType::GNUSparse => {
                if let Some(parent) = dest.parent() {
                    if !parent.as_os_str().is_empty() {
                        let relative_parent =
                            relative_dest.parent().unwrap_or_else(|| Path::new(""));
                        ensure_relative_dir_exists(directory, relative_parent, &mut known_dirs)?;
                    }
                }
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents).map_err(|e| {
                    io::Error::new(e.kind(), format!("read {}: {}", orig_path.display(), e))
                })?;
                fs::write(&dest, contents).map_err(|e| {
                    io::Error::new(e.kind(), format!("write {}: {}", dest.display(), e))
                })?;
            }
            tar::EntryType::Symlink => {
                if let Some(target) = entry.link_name()? {
                    if let Some(parent) = dest.parent() {
                        if !parent.as_os_str().is_empty() {
                            let relative_parent =
                                relative_dest.parent().unwrap_or_else(|| Path::new(""));
                            ensure_relative_dir_exists(
                                directory,
                                relative_parent,
                                &mut known_dirs,
                            )?;
                        }
                    }
                    #[allow(deprecated)]
                    let _ = std::fs::soft_link(target.as_ref(), &dest);
                }
            }
            _ => {
                // Skip hard links, char/block devices, etc.
            }
        }
    }

    Ok(())
}

fn resolve_disk_path(directory: Option<&str>, path: &Path) -> PathBuf {
    match directory {
        Some(base) if !path.is_absolute() => Path::new(base).join(path),
        _ => path.to_path_buf(),
    }
}

fn resolve_output_path(directory: Option<&str>, path: &Path) -> PathBuf {
    match directory {
        Some(base) if path.is_relative() => Path::new(base).join(path),
        _ => path.to_path_buf(),
    }
}

fn ensure_relative_dir_exists(
    directory: Option<&str>,
    relative_path: &Path,
    known_dirs: &mut HashSet<PathBuf>,
) -> io::Result<()> {
    let mut current = match directory {
        Some(base) => PathBuf::from(base),
        None => PathBuf::new(),
    };

    for component in relative_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                current.push(part);
                if known_dirs.contains(&current) {
                    continue;
                }
                match fs::create_dir(&current) {
                    Ok(()) => {
                        known_dirs.insert(current.clone());
                    }
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        known_dirs.insert(current.clone());
                    }
                    Err(err) => {
                        return Err(io::Error::new(
                            err.kind(),
                            format!("create_dir {}: {}", current.display(), err),
                        ));
                    }
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unsupported path component in {}", relative_path.display()),
                ));
            }
        }
    }

    Ok(())
}

fn do_list(archive_file: Option<&str>, gzip: bool, verbose: bool) -> io::Result<()> {
    let reader = open_read(archive_file, gzip)?;
    let mut archive = tar::Archive::new(reader);

    for entry_result in archive.entries()? {
        let entry = entry_result?;
        let path = entry.path()?;

        if verbose {
            let h = entry.header();
            let size = h.size().unwrap_or(0);
            let mode = h.mode().unwrap_or(0o644);
            let type_ch = match h.entry_type() {
                tar::EntryType::Directory => 'd',
                tar::EntryType::Symlink => 'l',
                _ => '-',
            };
            println!(
                "{}{} {:>8} {}",
                type_ch,
                format_mode(mode),
                size,
                path.display()
            );
        } else {
            println!("{}", path.display());
        }
    }

    Ok(())
}

fn open_read(archive_file: Option<&str>, gzip: bool) -> io::Result<Box<dyn Read>> {
    let reader: Box<dyn Read> = match archive_file {
        Some("-") | None => Box::new(io::stdin()),
        Some(path) => Box::new(File::open(path)?),
    };

    if gzip {
        Ok(Box::new(GzDecoder::new(reader)))
    } else {
        Ok(reader)
    }
}

fn write_archive_bytes(archive_file: Option<&str>, bytes: &[u8]) -> io::Result<()> {
    match archive_file {
        Some("-") | None => {
            let mut stdout = io::stdout();
            stdout.write_all(bytes)?;
            stdout.flush()
        }
        Some(path) => fs::write(path, bytes),
    }
}

fn strip_path_components(path: &Path, n: usize) -> Option<PathBuf> {
    let mut remaining = n;
    let mut stripped = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if remaining == 0 {
                    stripped.push(component.as_os_str());
                }
            }
            Component::Normal(part) => {
                if remaining > 0 {
                    remaining -= 1;
                } else {
                    stripped.push(part);
                }
            }
        }
    }

    if stripped.as_os_str().is_empty() {
        None
    } else {
        Some(stripped)
    }
}

fn format_mode(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for &(bit, ch) in &[
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ] {
        s.push(if mode & bit != 0 { ch } else { '-' });
    }
    s
}

fn print_usage() {
    eprintln!("Usage: tar [options] [files...]");
    eprintln!("  -c              create archive");
    eprintln!("  -x              extract archive");
    eprintln!("  -t              list archive contents");
    eprintln!("  -f FILE         archive filename (- for stdin/stdout)");
    eprintln!("  -z              gzip compress/decompress");
    eprintln!("  -v              verbose");
    eprintln!("  -C DIR          change directory");
    eprintln!("  --strip-components=N");
    eprintln!("                  strip N leading components on extract");
}
