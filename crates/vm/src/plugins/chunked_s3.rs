use crate::bridge::MountPluginContext;
use crate::metadata::CallbackMetadataStore;
use crate::plugins::s3_common::{
    create_s3_client, normalize_prefix, S3MountCredentials, DEFAULT_REGION,
};
use crate::protocol::OwnershipScope;

use agentos_vfs_core::engine::engines::{ChunkedFs, ChunkedFsOptions};
use agentos_vfs_core::engine::CachedMetadataStore;
use agentos_vfs_storage::MountedEngineFileSystem;
use agentos_vfs_storage::{S3BlockStore, S3BlockStoreOptions, SqliteMetadataStore};
use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::MountedFileSystem;
use serde::Deserialize;

const DEFAULT_METADATA_CACHE_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkedS3MountConfig {
    bucket: String,
    prefix: Option<String>,
    region: Option<String>,
    credentials: Option<S3MountCredentials>,
    endpoint: Option<String>,
    metadata_path: Option<String>,
    metadata_backend: Option<String>,
    mount_id: Option<String>,
    chunk_size: Option<u32>,
    inline_threshold: Option<usize>,
    uid: Option<u32>,
    gid: Option<u32>,
    file_mode: Option<u32>,
    dir_mode: Option<u32>,
    metadata_cache_entries: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct ChunkedS3MountPlugin;

pub(crate) trait ChunkedS3CallbackContext {
    fn chunked_s3_callback_context(
        &self,
    ) -> Option<(crate::state::SharedSidecarRequestClient, OwnershipScope)>;
}

impl ChunkedS3CallbackContext for () {
    fn chunked_s3_callback_context(
        &self,
    ) -> Option<(crate::state::SharedSidecarRequestClient, OwnershipScope)> {
        None
    }
}

impl<B> ChunkedS3CallbackContext for MountPluginContext<B> {
    fn chunked_s3_callback_context(
        &self,
    ) -> Option<(crate::state::SharedSidecarRequestClient, OwnershipScope)> {
        Some((
            self.sidecar_requests.clone(),
            OwnershipScope::vm(
                self.connection_id.clone(),
                self.session_id.clone(),
                self.vm_id.clone(),
            ),
        ))
    }
}

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for ChunkedS3MountPlugin {
    fn plugin_id(&self) -> &'static str {
        "chunked_s3"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ChunkedS3MountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let bucket = config.bucket.trim().to_owned();
        if bucket.is_empty() {
            return Err(PluginError::invalid_input(
                "chunked_s3 mount requires a non-empty bucket",
            ));
        }
        let metadata_backend = config.metadata_backend.as_deref().unwrap_or("sqlite");
        match metadata_backend {
            "sqlite" | "local" | "callback" => {}
            backend => {
                return Err(PluginError::invalid_input(format!(
                    "unsupported chunked_s3 metadataBackend: {backend}"
                )));
            }
        }

        let chunk_size = config
            .chunk_size
            .unwrap_or(agentos_vfs_core::engine::DEFAULT_CHUNK_SIZE);
        if chunk_size == 0 {
            return Err(PluginError::invalid_input(
                "chunked_s3 mount requires chunkSize to be greater than zero",
            ));
        }
        let inline_threshold = config
            .inline_threshold
            .unwrap_or(agentos_vfs_core::engine::DEFAULT_INLINE_THRESHOLD);
        if inline_threshold > chunk_size as usize {
            return Err(PluginError::invalid_input(
                "chunked_s3 mount requires inlineThreshold to be less than or equal to chunkSize",
            ));
        }

        let prefix = normalize_prefix(config.prefix.as_deref());
        let client = create_s3_client(
            config.region.unwrap_or_else(|| DEFAULT_REGION.to_owned()),
            config.endpoint,
            config.credentials,
        )?;
        let block_store = S3BlockStore::with_options(
            client,
            bucket,
            S3BlockStoreOptions {
                prefix: format!("{prefix}blocks/"),
            },
        );
        let cache_entries = config
            .metadata_cache_entries
            .unwrap_or(DEFAULT_METADATA_CACHE_ENTRIES);
        let options = ChunkedFsOptions {
            inline_threshold,
            chunk_size,
            uid: config.uid.unwrap_or(0),
            gid: config.gid.unwrap_or(0),
            file_mode: config.file_mode.unwrap_or(0o644),
            dir_mode: config.dir_mode.unwrap_or(0o755),
        };

        match metadata_backend {
            "callback" => {
                let (requests, ownership) = request
                    .context
                    .chunked_s3_callback_context()
                    .ok_or_else(|| {
                        PluginError::invalid_input(
                            "chunked_s3 callback metadata backend requires sidecar request context",
                        )
                    })?;
                let mount_id = config
                    .mount_id
                    .unwrap_or_else(|| request.guest_path.to_owned());
                let metadata = CachedMetadataStore::new(
                    CallbackMetadataStore::new(requests, ownership, mount_id),
                    cache_entries,
                );
                let fs = ChunkedFs::with_options(metadata, block_store, options);
                Ok(Box::new(MountedEngineFileSystem::with_runtime_context(
                    fs,
                    request.context.runtime_context.clone(),
                )))
            }
            "sqlite" | "local" => {
                let metadata_path = config.metadata_path.ok_or_else(|| {
                    PluginError::invalid_input("chunked_s3 sqlite metadata requires metadataPath")
                })?;
                let metadata = SqliteMetadataStore::open(metadata_path)
                    .map_err(|error| PluginError::new(error.code(), error.message().to_owned()))?;
                let metadata = CachedMetadataStore::new(metadata, cache_entries);
                let fs = ChunkedFs::with_options(metadata, block_store, options);
                Ok(Box::new(MountedEngineFileSystem::with_runtime_context(
                    fs,
                    request.context.runtime_context.clone(),
                )))
            }
            _ => unreachable!("metadata backend was validated above"),
        }
    }
}
