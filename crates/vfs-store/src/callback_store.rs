use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;
use vfs::engine::{
    BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs, DentryStat, InodeMeta, InodePatch,
    MetadataStore, SnapshotId, VfsError, VfsResult,
};

pub const VFS_METADATA_EXT_NAMESPACE: &str = "agentos.vfs.metadata.v1";
const CALLBACK_METADATA_TIMEOUT: Duration = Duration::from_secs(30);

pub trait CallbackMetadataClient: Clone + Send + Sync + 'static {
    type Ownership: Clone + Send + Sync + 'static;
    type Error: fmt::Display;

    fn invoke_metadata_callback(
        &self,
        ownership: Self::Ownership,
        namespace: &str,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<(String, Vec<u8>), Self::Error>;
}

#[derive(Clone)]
pub struct CallbackMetadataStore<C>
where
    C: CallbackMetadataClient,
{
    requests: C,
    ownership: C::Ownership,
    mount_id: String,
}

impl<C> CallbackMetadataStore<C>
where
    C: CallbackMetadataClient,
{
    pub fn new(requests: C, ownership: C::Ownership, mount_id: String) -> Self {
        Self {
            requests,
            ownership,
            mount_id,
        }
    }

    fn invoke(&self, method: MetadataCallbackMethod) -> VfsResult<MetadataCallbackResponse> {
        let request = MetadataCallbackRequest {
            mount_id: self.mount_id.clone(),
            method,
        };
        let payload = serde_json::to_vec(&request).map_err(|error| {
            VfsError::eio(format!(
                "encode vfs metadata callback request for mount '{}': {error}",
                self.mount_id
            ))
        })?;
        let (namespace, payload) = self
            .requests
            .invoke_metadata_callback(
                self.ownership.clone(),
                VFS_METADATA_EXT_NAMESPACE,
                payload,
                CALLBACK_METADATA_TIMEOUT,
            )
            .map_err(Self::sidecar_error_to_vfs)?;
        if namespace != VFS_METADATA_EXT_NAMESPACE {
            return Err(VfsError::eio(format!(
                "unexpected vfs metadata callback namespace '{namespace}'"
            )));
        }
        let response: MetadataCallbackResponse =
            serde_json::from_slice(&payload).map_err(|error| {
                VfsError::eio(format!(
                    "decode vfs metadata callback response for mount '{}': {error}",
                    self.mount_id
                ))
            })?;
        if let MetadataCallbackResponse::Err { code, message } = &response {
            return Err(VfsError::new(code_from_string(code), message.clone()));
        }
        Ok(response)
    }

    fn sidecar_error_to_vfs(error: C::Error) -> VfsError {
        VfsError::eio(error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataCallbackRequest {
    pub mount_id: String,
    pub method: MetadataCallbackMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MetadataCallbackMethod {
    Resolve {
        path: String,
    },
    ResolveParent {
        path: String,
    },
    Lstat {
        path: String,
    },
    ListDir {
        ino: u64,
    },
    Create {
        parent: u64,
        name: String,
        attrs: CreateInodeAttrs,
    },
    Link {
        parent: u64,
        name: String,
        target: u64,
    },
    Remove {
        parent: u64,
        name: String,
    },
    Rename {
        src_parent: u64,
        src: String,
        dst_parent: u64,
        dst: String,
    },
    SetAttr {
        ino: u64,
        patch: InodePatch,
    },
    CommitWrite {
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    },
    GetChunks {
        ino: u64,
        range: ChunkRange,
    },
    Snapshot {
        root: u64,
    },
    Fork {
        snap: SnapshotId,
    },
    Gc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MetadataCallbackResponse {
    InodeMeta { meta: InodeMeta },
    ResolveParent { parent: InodeMeta, name: String },
    DentryStats { entries: Vec<DentryStat> },
    Unit,
    BlockKeys { keys: Vec<BlockKey> },
    ChunkRefs { chunks: Vec<ChunkRef> },
    Snapshot { id: SnapshotId },
    Inode { ino: u64 },
    Err { code: String, message: String },
}

#[async_trait]
impl<C> MetadataStore for CallbackMetadataStore<C>
where
    C: CallbackMetadataClient,
{
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta> {
        match self.invoke(MetadataCallbackMethod::Resolve {
            path: path.to_owned(),
        })? {
            MetadataCallbackResponse::InodeMeta { meta } => Ok(meta),
            other => Err(unexpected_response("resolve", other)),
        }
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)> {
        match self.invoke(MetadataCallbackMethod::ResolveParent {
            path: path.to_owned(),
        })? {
            MetadataCallbackResponse::ResolveParent { parent, name } => Ok((parent, name)),
            other => Err(unexpected_response("resolveParent", other)),
        }
    }

    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta> {
        match self.invoke(MetadataCallbackMethod::Lstat {
            path: path.to_owned(),
        })? {
            MetadataCallbackResponse::InodeMeta { meta } => Ok(meta),
            other => Err(unexpected_response("lstat", other)),
        }
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>> {
        match self.invoke(MetadataCallbackMethod::ListDir { ino })? {
            MetadataCallbackResponse::DentryStats { entries } => Ok(entries),
            other => Err(unexpected_response("listDir", other)),
        }
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta> {
        match self.invoke(MetadataCallbackMethod::Create {
            parent,
            name: name.to_owned(),
            attrs,
        })? {
            MetadataCallbackResponse::InodeMeta { meta } => Ok(meta),
            other => Err(unexpected_response("create", other)),
        }
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        match self.invoke(MetadataCallbackMethod::Link {
            parent,
            name: name.to_owned(),
            target,
        })? {
            MetadataCallbackResponse::Unit => Ok(()),
            other => Err(unexpected_response("link", other)),
        }
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        match self.invoke(MetadataCallbackMethod::Remove {
            parent,
            name: name.to_owned(),
        })? {
            MetadataCallbackResponse::BlockKeys { keys } => Ok(keys),
            other => Err(unexpected_response("remove", other)),
        }
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        match self.invoke(MetadataCallbackMethod::Rename {
            src_parent,
            src: src.to_owned(),
            dst_parent,
            dst: dst.to_owned(),
        })? {
            MetadataCallbackResponse::BlockKeys { keys } => Ok(keys),
            other => Err(unexpected_response("rename", other)),
        }
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        match self.invoke(MetadataCallbackMethod::SetAttr { ino, patch })? {
            MetadataCallbackResponse::BlockKeys { keys } => Ok(keys),
            other => Err(unexpected_response("setAttr", other)),
        }
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        match self.invoke(MetadataCallbackMethod::CommitWrite {
            ino,
            edits,
            new_size,
            allocated_extents,
        })? {
            MetadataCallbackResponse::BlockKeys { keys } => Ok(keys),
            other => Err(unexpected_response("commitWrite", other)),
        }
    }

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>> {
        match self.invoke(MetadataCallbackMethod::GetChunks { ino, range })? {
            MetadataCallbackResponse::ChunkRefs { chunks } => Ok(chunks),
            other => Err(unexpected_response("getChunks", other)),
        }
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        match self.invoke(MetadataCallbackMethod::Snapshot { root })? {
            MetadataCallbackResponse::Snapshot { id } => Ok(id),
            other => Err(unexpected_response("snapshot", other)),
        }
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        match self.invoke(MetadataCallbackMethod::Fork { snap })? {
            MetadataCallbackResponse::Inode { ino } => Ok(ino),
            other => Err(unexpected_response("fork", other)),
        }
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        match self.invoke(MetadataCallbackMethod::Gc)? {
            MetadataCallbackResponse::BlockKeys { keys } => Ok(keys),
            other => Err(unexpected_response("gc", other)),
        }
    }
}

fn unexpected_response(method: &str, response: MetadataCallbackResponse) -> VfsError {
    VfsError::eio(format!(
        "unexpected vfs metadata callback response for {method}: {response:?}"
    ))
}

fn code_from_string(code: &str) -> &'static str {
    match code {
        "ENOENT" => "ENOENT",
        "EEXIST" => "EEXIST",
        "ENOTDIR" => "ENOTDIR",
        "EISDIR" => "EISDIR",
        "ELOOP" => "ELOOP",
        "ENAMETOOLONG" => "ENAMETOOLONG",
        "ENOTEMPTY" => "ENOTEMPTY",
        "EOPNOTSUPP" => "EOPNOTSUPP",
        "EROFS" => "EROFS",
        "EINVAL" => "EINVAL",
        "EACCES" => "EACCES",
        "EPERM" => "EPERM",
        "ENOSYS" => "ENOSYS",
        _ => "EIO",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use vfs::engine::engines::{ChunkedFs, ChunkedFsOptions};
    use vfs::engine::mem::{InMemoryMetadataStore, MemoryBlockStore};
    use vfs::engine::{InodeType, MetadataStore, Storage, Timespec, VirtualFileSystem};

    #[derive(Default)]
    struct RecordingMetadataTransport {
        requests: Mutex<Vec<MetadataCallbackRequest>>,
    }

    impl CallbackMetadataClient for Arc<RecordingMetadataTransport> {
        type Ownership = String;
        type Error = String;

        fn invoke_metadata_callback(
            &self,
            _ownership: Self::Ownership,
            namespace: &str,
            payload: Vec<u8>,
            _timeout: Duration,
        ) -> Result<(String, Vec<u8>), Self::Error> {
            assert_eq!(namespace, VFS_METADATA_EXT_NAMESPACE);
            let callback_request: MetadataCallbackRequest =
                serde_json::from_slice(&payload).expect("decode metadata callback");
            self.requests
                .lock()
                .expect("lock request log")
                .push(callback_request);

            let now = Timespec { sec: 1, nsec: 2 };
            let response = MetadataCallbackResponse::InodeMeta {
                meta: InodeMeta {
                    ino: 7,
                    kind: InodeType::File,
                    mode: 0o644,
                    uid: 1000,
                    gid: 1000,
                    size: 5,
                    nlink: 1,
                    atime: now,
                    mtime: now,
                    ctime: now,
                    birthtime: now,
                    storage: Storage::Inline(b"hello".to_vec()),
                    symlink_target: None,
                    allocated_extents: vec![(0, 1)],
                    xattrs: BTreeMap::new(),
                },
            };
            let payload = serde_json::to_vec(&response).expect("encode metadata callback response");
            Ok((VFS_METADATA_EXT_NAMESPACE.to_string(), payload))
        }
    }

    #[tokio::test]
    async fn callback_metadata_store_sends_typed_ext_requests() {
        let transport = Arc::new(RecordingMetadataTransport::default());
        let store = CallbackMetadataStore::new(
            transport.clone(),
            "vm-owner".to_string(),
            "mount-a".to_string(),
        );

        let meta = store
            .resolve("/file.txt")
            .await
            .expect("resolve via callback");
        assert_eq!(meta.ino, 7);
        assert_eq!(meta.storage, Storage::Inline(b"hello".to_vec()));

        let requests = transport.requests.lock().expect("lock request log");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].mount_id, "mount-a");
        match &requests[0].method {
            MetadataCallbackMethod::Resolve { path } => assert_eq!(path, "/file.txt"),
            other => panic!("unexpected method: {other:?}"),
        }
    }

    #[derive(Default)]
    struct DelegatingMetadataTransport {
        inner: InMemoryMetadataStore,
        methods: Mutex<Vec<&'static str>>,
    }

    impl CallbackMetadataClient for Arc<DelegatingMetadataTransport> {
        type Ownership = String;
        type Error = String;

        fn invoke_metadata_callback(
            &self,
            _ownership: Self::Ownership,
            namespace: &str,
            payload: Vec<u8>,
            _timeout: Duration,
        ) -> Result<(String, Vec<u8>), Self::Error> {
            assert_eq!(namespace, VFS_METADATA_EXT_NAMESPACE);
            let request: MetadataCallbackRequest =
                serde_json::from_slice(&payload).map_err(|err| err.to_string())?;
            let inner = self.inner.clone();
            let response = std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| err.to_string())?;
                runtime.block_on(handle_metadata_callback(inner, request.method))
            })
            .join()
            .map_err(|_| "metadata callback thread panicked".to_string())??;
            self.methods
                .lock()
                .expect("lock method log")
                .push(method_name(&response));
            let payload = serde_json::to_vec(&response).map_err(|err| err.to_string())?;
            Ok((VFS_METADATA_EXT_NAMESPACE.to_string(), payload))
        }
    }

    async fn handle_metadata_callback(
        inner: InMemoryMetadataStore,
        method: MetadataCallbackMethod,
    ) -> Result<MetadataCallbackResponse, String> {
        let result = match method {
            MetadataCallbackMethod::Resolve { path } => inner
                .resolve(&path)
                .await
                .map(|meta| MetadataCallbackResponse::InodeMeta { meta }),
            MetadataCallbackMethod::ResolveParent { path } => inner
                .resolve_parent(&path)
                .await
                .map(|(parent, name)| MetadataCallbackResponse::ResolveParent { parent, name }),
            MetadataCallbackMethod::Lstat { path } => inner
                .lstat(&path)
                .await
                .map(|meta| MetadataCallbackResponse::InodeMeta { meta }),
            MetadataCallbackMethod::ListDir { ino } => inner
                .list_dir(ino)
                .await
                .map(|entries| MetadataCallbackResponse::DentryStats { entries }),
            MetadataCallbackMethod::Create {
                parent,
                name,
                attrs,
            } => inner
                .create(parent, &name, attrs)
                .await
                .map(|meta| MetadataCallbackResponse::InodeMeta { meta }),
            MetadataCallbackMethod::Link {
                parent,
                name,
                target,
            } => inner
                .link(parent, &name, target)
                .await
                .map(|()| MetadataCallbackResponse::Unit),
            MetadataCallbackMethod::Remove { parent, name } => inner
                .remove(parent, &name)
                .await
                .map(|keys| MetadataCallbackResponse::BlockKeys { keys }),
            MetadataCallbackMethod::Rename {
                src_parent,
                src,
                dst_parent,
                dst,
            } => inner
                .rename(src_parent, &src, dst_parent, &dst)
                .await
                .map(|keys| MetadataCallbackResponse::BlockKeys { keys }),
            MetadataCallbackMethod::SetAttr { ino, patch } => inner
                .set_attr(ino, patch)
                .await
                .map(|keys| MetadataCallbackResponse::BlockKeys { keys }),
            MetadataCallbackMethod::CommitWrite {
                ino,
                edits,
                new_size,
                allocated_extents,
            } => inner
                .commit_write(ino, edits, new_size, allocated_extents)
                .await
                .map(|keys| MetadataCallbackResponse::BlockKeys { keys }),
            MetadataCallbackMethod::GetChunks { ino, range } => inner
                .get_chunks(ino, range)
                .await
                .map(|chunks| MetadataCallbackResponse::ChunkRefs { chunks }),
            MetadataCallbackMethod::Snapshot { root } => inner
                .snapshot(root)
                .await
                .map(|id| MetadataCallbackResponse::Snapshot { id }),
            MetadataCallbackMethod::Fork { snap } => inner
                .fork(snap)
                .await
                .map(|ino| MetadataCallbackResponse::Inode { ino }),
            MetadataCallbackMethod::Gc => inner
                .gc()
                .await
                .map(|keys| MetadataCallbackResponse::BlockKeys { keys }),
        };
        Ok(
            result.unwrap_or_else(|error| MetadataCallbackResponse::Err {
                code: error.code().to_string(),
                message: error.message().to_string(),
            }),
        )
    }

    fn method_name(response: &MetadataCallbackResponse) -> &'static str {
        match response {
            MetadataCallbackResponse::InodeMeta { .. } => "inodeMeta",
            MetadataCallbackResponse::ResolveParent { .. } => "resolveParent",
            MetadataCallbackResponse::DentryStats { .. } => "dentryStats",
            MetadataCallbackResponse::Unit => "unit",
            MetadataCallbackResponse::BlockKeys { .. } => "blockKeys",
            MetadataCallbackResponse::ChunkRefs { .. } => "chunkRefs",
            MetadataCallbackResponse::Snapshot { .. } => "snapshot",
            MetadataCallbackResponse::Inode { .. } => "inode",
            MetadataCallbackResponse::Err { .. } => "err",
        }
    }

    #[tokio::test]
    async fn callback_metadata_store_drives_chunked_filesystem_round_trip() {
        let transport = Arc::new(DelegatingMetadataTransport::default());
        let metadata = CallbackMetadataStore::new(
            transport.clone(),
            "vm-owner".to_string(),
            "mount-a".to_string(),
        );
        let fs = ChunkedFs::with_options(
            metadata,
            MemoryBlockStore::new(),
            ChunkedFsOptions {
                inline_threshold: 1,
                chunk_size: 4,
                ..ChunkedFsOptions::default()
            },
        );

        fs.write_file("/file.txt", b"abcdefgh").await.unwrap();
        fs.pwrite("/file.txt", b"YY", 2).await.unwrap();
        assert_eq!(fs.read_file("/file.txt").await.unwrap(), b"abYYefgh");
        fs.truncate("/file.txt", 5).await.unwrap();
        assert_eq!(fs.read_file("/file.txt").await.unwrap(), b"abYYe");

        let methods = transport.methods.lock().expect("lock method log");
        assert!(methods.contains(&"blockKeys"));
        assert!(methods.contains(&"chunkRefs"));
    }
}
