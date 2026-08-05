use crate::bridge::MountPluginContext;
use agentos_vfs_core::engine::engines::{ChunkedFs, ChunkedFsOptions};
use agentos_vfs_core::engine::CachedMetadataStore;
use agentos_vfs_storage::MountedEngineFileSystem;
use agentos_vfs_storage::{FileBlockStore, SqliteMetadataStore};
use agentos_vm_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agentos_vm_kernel::mount_table::MountedFileSystem;
use serde::Deserialize;

const DEFAULT_METADATA_CACHE_ENTRIES: usize = 4096;
const LOCAL_CHUNK_SIZE: u32 = 8 * 1024;
const LOCAL_INLINE_THRESHOLD: usize = 4 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkedLocalMountConfig {
    metadata_path: String,
    block_root: String,
    chunk_size: Option<u32>,
    inline_threshold: Option<usize>,
    uid: Option<u32>,
    gid: Option<u32>,
    file_mode: Option<u32>,
    dir_mode: Option<u32>,
    metadata_cache_entries: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct ChunkedLocalMountPlugin;

impl<B> FileSystemPluginFactory<MountPluginContext<B>> for ChunkedLocalMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "chunked_local"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, MountPluginContext<B>>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: ChunkedLocalMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        if config.metadata_path.trim().is_empty() {
            return Err(PluginError::invalid_input(
                "chunked_local mount requires metadataPath",
            ));
        }
        if config.block_root.trim().is_empty() {
            return Err(PluginError::invalid_input(
                "chunked_local mount requires blockRoot",
            ));
        }

        let chunk_size = config.chunk_size.unwrap_or(LOCAL_CHUNK_SIZE);
        if chunk_size == 0 {
            return Err(PluginError::invalid_input(
                "chunked_local mount requires chunkSize to be greater than zero",
            ));
        }
        let inline_threshold = config.inline_threshold.unwrap_or(LOCAL_INLINE_THRESHOLD);
        if inline_threshold > chunk_size as usize {
            return Err(PluginError::invalid_input(
                "chunked_local mount requires inlineThreshold to be less than or equal to chunkSize",
            ));
        }

        let metadata = SqliteMetadataStore::open(config.metadata_path)
            .map_err(|error| PluginError::new(error.code(), error.message().to_owned()))?;
        let metadata = CachedMetadataStore::new(
            metadata,
            config
                .metadata_cache_entries
                .unwrap_or(DEFAULT_METADATA_CACHE_ENTRIES),
        );
        let blocks = FileBlockStore::new(config.block_root)
            .map_err(|error| PluginError::new(error.code(), error.message().to_owned()))?;
        let fs = ChunkedFs::with_adaptive_chunk_size(
            metadata,
            blocks,
            ChunkedFsOptions {
                inline_threshold,
                chunk_size,
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
