#![allow(dead_code)] // Retained while user-facing registration is intentionally disabled in mod.rs.

use crate::bridge::MountPluginContext;
use crate::plugins::s3_common::{
    create_s3_client, normalize_prefix, S3MountCredentials, DEFAULT_REGION,
};

use agentos_vfs_core::engine::engines::{ObjectFs, ObjectFsOptions};
use agentos_vfs_storage::MountedEngineFileSystem;
use agentos_vfs_storage::{S3ObjectBackend, S3ObjectBackendOptions};
use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::MountedFileSystem;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ObjectS3MountConfig {
    bucket: String,
    prefix: Option<String>,
    region: Option<String>,
    credentials: Option<S3MountCredentials>,
    endpoint: Option<String>,
    uid: Option<u32>,
    gid: Option<u32>,
    file_mode: Option<u32>,
    dir_mode: Option<u32>,
}

#[derive(Debug)]
pub(crate) struct ObjectS3MountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for ObjectS3MountPlugin {
    fn plugin_id(&self) -> &'static str {
        "object_s3"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ObjectS3MountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let bucket = config.bucket.trim().to_owned();
        if bucket.is_empty() {
            return Err(PluginError::invalid_input(
                "object_s3 mount requires a non-empty bucket",
            ));
        }

        let prefix = normalize_prefix(config.prefix.as_deref());
        let client = create_s3_client(
            config.region.unwrap_or_else(|| DEFAULT_REGION.to_owned()),
            config.endpoint,
            config.credentials,
        )?;
        let backend = S3ObjectBackend::with_options(
            client,
            bucket,
            S3ObjectBackendOptions {
                prefix: prefix.clone(),
            },
        );
        let fs = ObjectFs::with_options(
            backend,
            ObjectFsOptions {
                prefix: String::new(),
                uid: config.uid.unwrap_or(0),
                gid: config.gid.unwrap_or(0),
                file_mode: config.file_mode.unwrap_or(0o644),
                dir_mode: config.dir_mode.unwrap_or(0o755),
            },
        );
        Ok(Box::new(MountedEngineFileSystem::with_runtime_context(
            fs,
            request.context.runtime_context.clone(),
        )))
    }
}
