//! Guest-native `/opt/agentos` package projection.
//!
//! Package files are mounted directly from the uncompressed package tar, not
//! extracted into a host staging tree. Transition fixtures can still mount an
//! unpacked package dir read-only under this plugin id. Tar packages remain on
//! the no-extraction path: the VFS scans headers once, then serves reads by
//! mmap-backed byte range.
//!
//! The crucial difference from a plain `host_dir` mount is the PLUGIN ID: module
//! resolution and path translation only treat mounts classified
//! `host_dir`/`module_access` as host-backed (`build_module_reader` /
//! `runtime_guest_path_mappings` in `execution.rs`). Because this mount is
//! `agentos_packages`, the JS runtime resolves `/opt/agentos` modules through the
//! kernel VFS — no host↔guest path translation (the `/unknown/<cmd>` failure
//! mode).
//!
//! Projection uses granular leaf mounts: one tar mount for
//! `/opt/agentos/pkgs/<pkg>/<version>`, one synthetic root symlink for
//! `pkgs/<pkg>/current`, and one synthetic root symlink per managed command or
//! manpage. The parent directories remain writable overlay directories so
//! guest-installed commands can coexist with managed package entries.

use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use serde::Deserialize;

use crate::plugins::host_dir::{HostDirFilesystem, HostDirReadLimitContext};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum AgentosPackagesMountConfig {
    Tar {
        #[serde(rename = "tarPath")]
        tar_path: String,
        #[serde(default)]
        root: Option<String>,
        #[serde(rename = "readOnly")]
        read_only: Option<bool>,
    },
    HostDir {
        #[serde(rename = "hostPath")]
        host_path: String,
        #[serde(rename = "readOnly")]
        read_only: Option<bool>,
    },
    SingleSymlink {
        target: String,
        #[serde(rename = "readOnly")]
        read_only: Option<bool>,
    },
}

#[derive(Debug)]
pub(crate) struct AgentosPackagesMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for AgentosPackagesMountPlugin
where
    Context: HostDirReadLimitContext,
{
    fn plugin_id(&self) -> &'static str {
        "agentos_packages"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: AgentosPackagesMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        match config {
            AgentosPackagesMountConfig::Tar {
                tar_path,
                root,
                read_only,
            } => {
                let filesystem = agentos_vfs_core::posix::TarFileSystem::open_at(
                    &tar_path,
                    root.as_deref().unwrap_or("/"),
                )
                .map_err(|error| PluginError::invalid_input(error.to_string()))?;
                let mounted = MountedVirtualFileSystem::new(filesystem);
                if read_only.unwrap_or(true) {
                    Ok(Box::new(ReadOnlyFileSystem::new(mounted)))
                } else {
                    Ok(Box::new(mounted))
                }
            }
            AgentosPackagesMountConfig::HostDir {
                host_path,
                read_only,
            } => {
                let filesystem = HostDirFilesystem::new_with_read_limit(
                    &host_path,
                    request.context.host_dir_max_read_bytes(),
                )
                .map_err(|error| PluginError::invalid_input(error.to_string()))?;
                let mounted = MountedVirtualFileSystem::new(filesystem);
                if read_only.unwrap_or(true) {
                    Ok(Box::new(ReadOnlyFileSystem::new(mounted)))
                } else {
                    Ok(Box::new(mounted))
                }
            }
            AgentosPackagesMountConfig::SingleSymlink { target, read_only } => {
                let mounted = MountedVirtualFileSystem::new(
                    agentos_vfs_core::posix::SingleSymlinkFileSystem::new(target),
                );
                if read_only.unwrap_or(true) {
                    Ok(Box::new(ReadOnlyFileSystem::new(mounted)))
                } else {
                    Ok(Box::new(mounted))
                }
            }
        }
    }
}
