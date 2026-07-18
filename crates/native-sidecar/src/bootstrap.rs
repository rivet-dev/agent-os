//! Root filesystem bootstrap and snapshot helpers extracted from vm.rs.

use crate::protocol::RootFilesystemEntry;
use crate::state::SidecarKernel;
use crate::SidecarError;

use agentos_kernel::root_fs::{FilesystemEntry as KernelFilesystemEntry, RootFilesystemSnapshot};
use agentos_kernel::vfs::VirtualFileSystem;
use std::collections::BTreeSet;

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KernelCommandInventory {
    pub(crate) names: BTreeSet<String>,
    pub(crate) search_roots: Vec<String>,
}

/// Enumerate legacy command mounts from the live kernel VFS for one immediate
/// registration/PATH rebuild. Nothing returned here is retained as pathname
/// authority; launch resolution revalidates the selected file in the kernel.
pub(crate) fn discover_kernel_commands(kernel: &mut SidecarKernel) -> KernelCommandInventory {
    let mut inventory = KernelCommandInventory::default();
    let Ok(command_roots) = kernel.read_dir("/__agentos/commands") else {
        return inventory;
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

        let mut root_has_commands = false;
        for entry in entries {
            if entry.starts_with('.') || entry.contains('/') {
                continue;
            }
            let candidate = format!("{guest_root}/{entry}");
            let Some(canonical) = kernel.realpath(&candidate).ok() else {
                continue;
            };
            let Some(stat) = kernel.lstat(&canonical).ok() else {
                continue;
            };
            if stat.is_directory || stat.is_symbolic_link {
                continue;
            }
            root_has_commands = true;
            inventory.names.insert(entry);
        }
        if root_has_commands {
            inventory.search_roots.push(guest_root);
        }
    }

    inventory
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentos_kernel::kernel::KernelVmConfig;
    use agentos_kernel::mount_table::MountTable;
    use agentos_kernel::permissions::Permissions;
    use agentos_kernel::vfs::MemoryFileSystem;

    fn test_kernel() -> SidecarKernel {
        let mut config = KernelVmConfig::new("vm-transient-command-discovery");
        config.permissions = Permissions::allow_all();
        SidecarKernel::new(MountTable::new(MemoryFileSystem::new()), config)
    }

    #[test]
    fn kernel_command_inventory_tracks_live_files_and_roots() {
        let mut kernel = test_kernel();
        kernel
            .mkdir("/__secure_exec/commands/001", true)
            .expect("create first command root");
        kernel
            .mkdir("/__secure_exec/commands/002/directory", true)
            .expect("create non-command directory");
        kernel
            .write_file(
                "/__secure_exec/commands/001/alpha",
                b"#!/usr/bin/env node\n".to_vec(),
            )
            .expect("write command");
        kernel
            .write_file(
                "/__secure_exec/commands/001/.hidden",
                b"#!/usr/bin/env node\n".to_vec(),
            )
            .expect("write hidden entry");

        let discovered = discover_kernel_commands(&mut kernel);
        assert_eq!(discovered.names, BTreeSet::from([String::from("alpha")]));
        assert_eq!(
            discovered.search_roots,
            vec![String::from("/__secure_exec/commands/001")]
        );

        kernel
            .remove_file("/__secure_exec/commands/001/alpha")
            .expect("remove command after initial discovery");
        assert_eq!(
            discover_kernel_commands(&mut kernel),
            KernelCommandInventory::default(),
            "transient discovery must not retain deleted commands or roots"
        );
    }
}
