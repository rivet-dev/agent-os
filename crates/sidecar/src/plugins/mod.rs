use crate::bridge::MountPluginContext;

use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, FileSystemPluginRegistry, PluginError,
};

pub(crate) mod google_drive;
pub(crate) mod host_dir;
pub(crate) mod js_bridge;
pub(crate) mod module_access;
pub(crate) mod s3;
pub(crate) mod sandbox_agent;

use google_drive::GoogleDriveMountPlugin;
use host_dir::HostDirMountPlugin;
use js_bridge::JsBridgeMountPlugin;
use module_access::ModuleAccessMountPlugin;
use s3::S3MountPlugin;
use sandbox_agent::SandboxAgentMountPlugin;

pub(crate) trait SidecarMountPluginFactory<Context>:
    FileSystemPluginFactory<Context>
{
}

impl<Context, T> SidecarMountPluginFactory<Context> for T where T: FileSystemPluginFactory<Context> {}

fn register_plugin<Context>(
    registry: &mut FileSystemPluginRegistry<Context>,
    plugin: impl SidecarMountPluginFactory<Context> + 'static,
) -> Result<(), PluginError> {
    registry.register(plugin)
}

pub(crate) fn register_native_mount_plugins<B>(
    registry: &mut FileSystemPluginRegistry<MountPluginContext<B>>,
) -> Result<(), PluginError> {
    register_plugin(registry, HostDirMountPlugin)?;
    register_plugin(registry, ModuleAccessMountPlugin)?;
    register_plugin(registry, JsBridgeMountPlugin)?;
    register_plugin(registry, SandboxAgentMountPlugin)?;
    register_plugin(registry, S3MountPlugin)?;
    register_plugin(registry, GoogleDriveMountPlugin)?;
    Ok(())
}
