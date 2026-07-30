use crate::bridge::MountPluginContext;

use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, FileSystemPluginRegistry, PluginError,
};

pub(crate) mod agentos_packages;
pub(crate) mod chunked_actor_sqlite;
pub(crate) mod chunked_local;
pub(crate) mod chunked_s3;
pub(crate) mod google_drive;
pub(crate) mod host_dir;
pub(crate) mod js_bridge;
pub(crate) mod module_access;
pub(crate) mod object_s3;
pub(crate) mod s3_common;
pub(crate) mod sandbox_agent;

use agentos_packages::AgentosPackagesMountPlugin;
use chunked_actor_sqlite::ChunkedActorSqliteMountPlugin;
use chunked_local::ChunkedLocalMountPlugin;
use chunked_s3::ChunkedS3MountPlugin;
use google_drive::GoogleDriveMountPlugin;
use host_dir::HostDirMountPlugin;
use js_bridge::JsBridgeMountPlugin;
use module_access::ModuleAccessMountPlugin;
// `object_s3` deliberately remains compiled but unregistered. Its one-object-per-file
// design turns ordinary POSIX metadata, sparse, truncate, and random-write workloads
// into repeated whole-object downloads and replacements. A single applicable pathname
// test generated 3,992,055 S3 requests before exceeding two hours. Re-exposing it needs
// a bounded coherent dirty-inode cache whose reads, links, renames, unlinks, fsync/sync,
// eviction, unmount, and upload-error behavior is proven first. Keep the implementation
// and focused regressions intact so that work can resume without reconstructing it.
// use object_s3::ObjectS3MountPlugin;
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
    register_plugin(registry, AgentosPackagesMountPlugin)?;
    register_plugin(registry, HostDirMountPlugin)?;
    register_plugin(registry, ModuleAccessMountPlugin)?;
    register_plugin(registry, JsBridgeMountPlugin)?;
    register_plugin(registry, SandboxAgentMountPlugin)?;
    register_plugin(registry, ChunkedLocalMountPlugin)?;
    register_plugin(registry, ChunkedActorSqliteMountPlugin)?;
    // Temporarily dormant; see the detailed rationale beside the retained import above.
    // register_plugin(registry, ObjectS3MountPlugin)?;
    register_plugin(registry, ChunkedS3MountPlugin)?;
    register_plugin(registry, GoogleDriveMountPlugin)?;
    Ok(())
}
