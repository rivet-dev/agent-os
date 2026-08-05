use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();

    if let Some(exit_code) = try_simple_mv(&args) {
        std::process::exit(exit_code);
    }

    std::process::exit(uu_mv::uumain(args.into_iter()));
}

fn try_simple_mv(args: &[OsString]) -> Option<i32> {
    let operands = parse_plain_operands(args)?;
    if operands.len() < 2 {
        return None;
    }

    match run_simple_mv(&operands) {
        Ok(()) => Some(0),
        Err(err) => {
            eprintln!("mv: {}", err);
            Some(1)
        }
    }
}

fn parse_plain_operands(args: &[OsString]) -> Option<Vec<PathBuf>> {
    let mut operands = Vec::new();
    let mut literal = false;

    for arg in args.iter().skip(1) {
        let text = arg.to_string_lossy();
        if !literal && text == "--" {
            literal = true;
            continue;
        }
        if !literal && text.starts_with('-') && text != "-" {
            return None;
        }
        operands.push(PathBuf::from(arg));
    }

    Some(operands)
}

fn run_simple_mv(operands: &[PathBuf]) -> io::Result<()> {
    let (target, sources) = operands
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing destination"))?;
    let target_meta = metadata_if_exists(target)?;
    let dest_is_dir = sources.len() > 1 || target_meta.as_ref().is_some_and(fs::Metadata::is_dir);

    if sources.len() > 1 && !dest_is_dir {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("target '{}' is not a directory", target.display()),
        ));
    }

    for source in sources {
        let destination = if dest_is_dir {
            target.join(file_name(source)?)
        } else {
            target.to_path_buf()
        };
        move_path(source, &destination)?;
    }

    Ok(())
}

fn move_path(source: &Path, destination: &Path) -> io::Result<()> {
    if paths_are_same_existing_file(source, destination)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "'{}' and '{}' are the same file",
                source.display(),
                destination.display()
            ),
        ));
    }

    match fs::rename(source, destination) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AlreadyExists | io::ErrorKind::DirectoryNotEmpty
            ) =>
        {
            let detail = if error.kind() == io::ErrorKind::DirectoryNotEmpty {
                "Directory not empty"
            } else {
                "File exists"
            };
            return Err(io::Error::new(
                error.kind(),
                format!("cannot overwrite '{}': {detail}", destination.display()),
            ));
        }
        Err(error) => return Err(error),
    }

    let metadata = fs::symlink_metadata(source)?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        move_symlink(source, destination)
    } else if metadata.is_dir() {
        move_dir(source, destination)
    } else {
        move_file(source, destination, &metadata.permissions())
    }
}

fn move_file(source: &Path, destination: &Path, permissions: &fs::Permissions) -> io::Result<()> {
    remove_existing_non_dir(destination)?;
    fs::copy(source, destination)?;
    if let Err(error) = fs::set_permissions(destination, permissions.clone()) {
        if !is_ignorable_permission_copy_error(&error) {
            return Err(error);
        }
    }
    fs::remove_file(source)
}

fn move_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    remove_existing_non_dir(destination)?;
    let target = fs::read_link(source)?;
    #[allow(deprecated)]
    std::fs::soft_link(&target, destination)?;
    fs::remove_file(source)
}

fn move_dir(source: &Path, destination: &Path) -> io::Result<()> {
    if destination_resolves_inside_source(source, destination)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "cannot move '{}' to a subdirectory of itself, '{}'",
                source.display(),
                destination.display()
            ),
        ));
    }
    if metadata_if_exists(destination)?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "cannot overwrite '{}': directory already exists",
                destination.display()
            ),
        ));
    }

    fs::create_dir(destination)?;

    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let child_source = entry.path();
        let child_destination = destination.join(entry.file_name());
        move_path(&child_source, &child_destination)?;
    }

    fs::remove_dir(source)
}

fn remove_existing_non_dir(path: &Path) -> io::Result<()> {
    if let Some(metadata) = metadata_if_exists(path)? {
        if metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("cannot overwrite directory '{}'", path.display()),
            ));
        }
        fs::remove_file(path)?;
    }
    Ok(())
}

fn paths_are_same_existing_file(source: &Path, destination: &Path) -> io::Result<bool> {
    if metadata_if_exists(destination)?.is_none() {
        return Ok(false);
    }

    let Some(source) = canonicalize_existing_path(source)? else {
        return Ok(false);
    };
    let Some(destination) = canonicalize_existing_path(destination)? else {
        return Ok(false);
    };

    Ok(source == destination)
}

fn canonicalize_existing_path(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(_) if fs::symlink_metadata(path)?.file_type().is_symlink() => Ok(None),
        Err(error) => Err(error),
    }
}

fn destination_resolves_inside_source(source: &Path, destination: &Path) -> io::Result<bool> {
    let source = fs::canonicalize(source)?;
    let Some(parent) = destination.parent() else {
        return Ok(false);
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let Some(parent) = canonical_existing_ancestor(parent)? else {
        return Ok(false);
    };

    Ok(parent.starts_with(source))
}

fn canonical_existing_ancestor(path: &Path) -> io::Result<Option<PathBuf>> {
    for ancestor in path.ancestors() {
        let ancestor = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        match fs::canonicalize(ancestor) {
            Ok(path) => return Ok(Some(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    Ok(None)
}

fn metadata_if_exists(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn file_name(path: &Path) -> io::Result<&OsStr> {
    path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot determine file name for '{}'", path.display()),
        )
    })
}

fn is_ignorable_permission_copy_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::Unsupported || matches!(error.raw_os_error(), Some(52 | 95))
}
