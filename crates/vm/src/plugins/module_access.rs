use crate::plugins::host_dir::{HostDirFilesystem, HostDirReadLimitContext};

use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleAccessMountConfig {
    host_path: String,
}

#[derive(Debug)]
pub(crate) struct ModuleAccessMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for ModuleAccessMountPlugin
where
    Context: HostDirReadLimitContext,
{
    fn plugin_id(&self) -> &'static str {
        "module_access"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ModuleAccessMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let host_path = validate_module_access_root(&config.host_path)?;
        let filesystem = HostDirFilesystem::new_with_read_limit(
            &host_path,
            request.context.host_dir_max_read_bytes(),
        )
        .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        Ok(Box::new(ReadOnlyFileSystem::new(
            MountedVirtualFileSystem::new(filesystem),
        )))
    }
}

fn validate_module_access_root(path: &str) -> Result<PathBuf, PluginError> {
    let root = fs::canonicalize(path).map_err(|error| {
        PluginError::invalid_input(format!(
            "failed to resolve module_access root {path}: {error}"
        ))
    })?;
    if root.file_name() == Some(Path::new("node_modules").as_os_str()) {
        return Ok(root);
    }

    Err(PluginError::invalid_input(format!(
        "module_access roots must resolve to a node_modules directory: {path}"
    )))
}
