//! Root filesystem bootstrap and snapshot helpers extracted from vm.rs.

use crate::protocol::{
    RootFilesystemDescriptor, RootFilesystemEntry, RootFilesystemEntryEncoding,
    RootFilesystemEntryKind, RootFilesystemLowerDescriptor, RootFilesystemMode,
};
use crate::service::{
    dirname, encode_guest_filesystem_content, normalize_path, root_filesystem_error, vfs_error,
};
use crate::state::SidecarKernel;
use crate::SidecarError;

use agent_os_bridge::FilesystemSnapshot;
use agent_os_kernel::root_fs::{
    decode_snapshot as decode_root_snapshot, FilesystemEntry as KernelFilesystemEntry,
    FilesystemEntryKind as KernelFilesystemEntryKind, RootFileSystem,
    RootFilesystemDescriptor as KernelRootFilesystemDescriptor,
    RootFilesystemMode as KernelRootFilesystemMode, RootFilesystemSnapshot,
    ROOT_FILESYSTEM_SNAPSHOT_FORMAT,
};
use agent_os_kernel::vfs::VirtualFileSystem;
use base64::Engine;
use std::collections::BTreeMap;

pub(crate) fn build_root_filesystem(
    descriptor: &RootFilesystemDescriptor,
    loaded_snapshot: Option<&FilesystemSnapshot>,
) -> Result<RootFileSystem, SidecarError> {
    let restored_snapshot = match loaded_snapshot {
        Some(snapshot) if snapshot.format == ROOT_FILESYSTEM_SNAPSHOT_FORMAT => {
            Some(decode_root_snapshot(&snapshot.bytes).map_err(root_filesystem_error)?)
        }
        _ => None,
    };
    let has_restored_snapshot = restored_snapshot.is_some();

    let lowers = if let Some(snapshot) = restored_snapshot {
        vec![snapshot]
    } else {
        descriptor
            .lowers
            .iter()
            .map(convert_root_lower_descriptor)
            .collect::<Result<Vec<_>, _>>()?
    };

    RootFileSystem::from_descriptor(KernelRootFilesystemDescriptor {
        mode: match descriptor.mode {
            RootFilesystemMode::Ephemeral => KernelRootFilesystemMode::Ephemeral,
            RootFilesystemMode::ReadOnly => KernelRootFilesystemMode::ReadOnly,
        },
        disable_default_base_layer: has_restored_snapshot || descriptor.disable_default_base_layer,
        lowers,
        bootstrap_entries: descriptor
            .bootstrap_entries
            .iter()
            .map(convert_root_filesystem_entry)
            .collect::<Result<Vec<_>, _>>()?,
    })
    .map_err(root_filesystem_error)
}

pub(crate) fn root_snapshot_entry(entry: &KernelFilesystemEntry) -> RootFilesystemEntry {
    let (content, encoding) = match entry.content.as_ref() {
        Some(bytes) => {
            let (content, encoding) = encode_guest_filesystem_content(bytes.clone());
            (Some(content), Some(encoding))
        }
        None => (None, None),
    };

    RootFilesystemEntry {
        path: entry.path.clone(),
        kind: match entry.kind {
            KernelFilesystemEntryKind::File => RootFilesystemEntryKind::File,
            KernelFilesystemEntryKind::Directory => RootFilesystemEntryKind::Directory,
            KernelFilesystemEntryKind::Symlink => RootFilesystemEntryKind::Symlink,
        },
        mode: Some(entry.mode),
        uid: Some(entry.uid),
        gid: Some(entry.gid),
        content,
        encoding,
        target: entry.target.clone(),
        executable: matches!(entry.kind, KernelFilesystemEntryKind::File)
            && (entry.mode & 0o111) != 0,
    }
}

pub(crate) fn apply_root_filesystem_entry<F>(
    filesystem: &mut F,
    entry: &RootFilesystemEntry,
) -> Result<(), SidecarError>
where
    F: VirtualFileSystem,
{
    let kernel_entry = convert_root_filesystem_entry(entry)?;
    ensure_parent_directories(filesystem, &kernel_entry.path)?;

    match kernel_entry.kind {
        KernelFilesystemEntryKind::Directory => filesystem
            .mkdir(&kernel_entry.path, true)
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::File => filesystem
            .write_file(&kernel_entry.path, kernel_entry.content.unwrap_or_default())
            .map_err(vfs_error)?,
        KernelFilesystemEntryKind::Symlink => filesystem
            .symlink(
                kernel_entry.target.as_deref().ok_or_else(|| {
                    SidecarError::InvalidState(format!(
                        "root filesystem bootstrap for symlink {} requires a target",
                        entry.path
                    ))
                })?,
                &kernel_entry.path,
            )
            .map_err(vfs_error)?,
    }

    if !matches!(kernel_entry.kind, KernelFilesystemEntryKind::Symlink) {
        filesystem
            .chmod(&kernel_entry.path, kernel_entry.mode)
            .map_err(vfs_error)?;
        filesystem
            .chown(&kernel_entry.path, kernel_entry.uid, kernel_entry.gid)
            .map_err(vfs_error)?;
    }

    Ok(())
}

pub(crate) fn discover_command_guest_paths(kernel: &mut SidecarKernel) -> BTreeMap<String, String> {
    let mut command_guest_paths = BTreeMap::new();
    let Ok(command_roots) = kernel.read_dir("/__agentos/commands") else {
        return command_guest_paths;
    };

    let mut ordered_roots = command_roots
        .into_iter()
        .filter(|entry| !entry.is_empty() && entry.chars().all(|ch| ch.is_ascii_digit()))
        .collect::<Vec<_>>();
    ordered_roots.sort();

    for root in ordered_roots {
        let guest_root = format!("/__agentos/commands/{root}");
        let Ok(entries) = kernel.read_dir(&guest_root) else {
            continue;
        };

        for entry in entries {
            if entry.starts_with('.') || command_guest_paths.contains_key(&entry) {
                continue;
            }
            command_guest_paths.insert(entry.clone(), format!("{guest_root}/{entry}"));
        }
    }

    command_guest_paths
}

fn convert_root_lower_descriptor(
    lower: &RootFilesystemLowerDescriptor,
) -> Result<RootFilesystemSnapshot, SidecarError> {
    match lower {
        RootFilesystemLowerDescriptor::Snapshot { entries } => Ok(RootFilesystemSnapshot {
            entries: entries
                .iter()
                .map(convert_root_filesystem_entry)
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

fn convert_root_filesystem_entry(
    entry: &RootFilesystemEntry,
) -> Result<KernelFilesystemEntry, SidecarError> {
    let mode = entry.mode.unwrap_or_else(|| match entry.kind {
        RootFilesystemEntryKind::File => {
            if entry.executable {
                0o755
            } else {
                0o644
            }
        }
        RootFilesystemEntryKind::Directory => 0o755,
        RootFilesystemEntryKind::Symlink => 0o777,
    });

    let content = match entry.content.as_ref() {
        Some(content) => match entry.encoding {
            Some(RootFilesystemEntryEncoding::Base64) => Some(
                base64::engine::general_purpose::STANDARD
                    .decode(content)
                    .map_err(|error| {
                        SidecarError::InvalidState(format!(
                            "invalid base64 root filesystem content for {}: {error}",
                            entry.path
                        ))
                    })?,
            ),
            Some(RootFilesystemEntryEncoding::Utf8) | None => Some(content.as_bytes().to_vec()),
        },
        None => None,
    };

    Ok(KernelFilesystemEntry {
        path: normalize_path(&entry.path),
        kind: match entry.kind {
            RootFilesystemEntryKind::File => KernelFilesystemEntryKind::File,
            RootFilesystemEntryKind::Directory => KernelFilesystemEntryKind::Directory,
            RootFilesystemEntryKind::Symlink => KernelFilesystemEntryKind::Symlink,
        },
        mode,
        uid: entry.uid.unwrap_or(0),
        gid: entry.gid.unwrap_or(0),
        content,
        target: entry.target.clone(),
    })
}

fn ensure_parent_directories<F>(filesystem: &mut F, path: &str) -> Result<(), SidecarError>
where
    F: VirtualFileSystem,
{
    let parent = dirname(path);
    if parent != "/" && !filesystem.exists(&parent) {
        filesystem.mkdir(&parent, true).map_err(vfs_error)?;
    }
    Ok(())
}
