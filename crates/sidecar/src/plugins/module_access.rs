use crate::plugins::host_dir::HostDirFilesystem;

use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{
    MountedFileSystem, MountedVirtualFileSystem, ReadOnlyFileSystem,
};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModuleAccessMountConfig {
    host_path: String,
}

#[derive(Debug)]
pub(crate) struct ModuleAccessMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for ModuleAccessMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "module_access"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ModuleAccessMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        validate_module_access_root(&config.host_path)?;
        let filesystem = HostDirFilesystem::new(&config.host_path)
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        Ok(Box::new(ReadOnlyFileSystem::new(MountedVirtualFileSystem::new(
            filesystem,
        ))))
    }
}

fn validate_module_access_root(path: &str) -> Result<(), PluginError> {
    let root = PathBuf::from(path);
    if root.file_name() == Some(Path::new("node_modules").as_os_str()) {
        return Ok(());
    }

    Err(PluginError::invalid_input(format!(
        "module_access roots must point at a node_modules directory: {path}"
    )))
}
