//! Root filesystem bootstrap and snapshot helpers extracted from vm.rs.

use crate::protocol::RootFilesystemEntry;
use crate::state::SidecarKernel;
use crate::SidecarError;

use agentos_kernel::root_fs::{FilesystemEntry as KernelFilesystemEntry, RootFilesystemSnapshot};
use agentos_kernel::vfs::VirtualFileSystem;
use std::collections::BTreeMap;

pub(crate) fn root_snapshot_entry(entry: &KernelFilesystemEntry) -> RootFilesystemEntry {
    agentos_native_sidecar_core::root_snapshot_entry(entry)
}

pub(crate) fn root_snapshot_entries(snapshot: &RootFilesystemSnapshot) -> Vec<RootFilesystemEntry> {
    snapshot.entries.iter().map(root_snapshot_entry).collect()
}

pub(crate) fn root_snapshot_from_entries(
    entries: &[RootFilesystemEntry],
) -> Result<RootFilesystemSnapshot, SidecarError> {
    agentos_native_sidecar_core::root_snapshot_from_entries(entries)
        .map_err(|error| SidecarError::InvalidState(error.to_string()))
}

pub(crate) fn apply_root_filesystem_entry<F>(
    filesystem: &mut F,
    entry: &RootFilesystemEntry,
) -> Result<(), SidecarError>
where
    F: VirtualFileSystem,
{
    agentos_native_sidecar_core::apply_root_filesystem_entry(filesystem, entry)
        .map_err(|error| SidecarError::InvalidState(error.to_string()))
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
