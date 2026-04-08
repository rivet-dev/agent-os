use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{MountedFileSystem, MountedVirtualFileSystem};
use agent_os_kernel::vfs::{
    MemoryFileSystem, MemoryFileSystemSnapshot, MemoryFileSystemSnapshotInode,
    MemoryFileSystemSnapshotInodeKind, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem,
    VirtualStat,
};
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::error::ProvideErrorMetadata;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use tokio::runtime::Runtime;
use url::Url;

const DEFAULT_CHUNK_SIZE: usize = 4 * 1024 * 1024;
const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024;
const MANIFEST_FORMAT: &str = "agent_os_s3_filesystem_manifest_v1";
const DEFAULT_REGION: &str = "us-east-1";
const MAX_PERSISTED_MANIFEST_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct S3MountCredentials {
    access_key_id: String,
    secret_access_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct S3MountConfig {
    bucket: String,
    prefix: Option<String>,
    region: Option<String>,
    credentials: Option<S3MountCredentials>,
    endpoint: Option<String>,
    chunk_size: Option<usize>,
    inline_threshold: Option<usize>,
}

#[derive(Debug)]
pub(crate) struct S3MountPlugin;

impl<Context> FileSystemPluginFactory<Context> for S3MountPlugin {
    fn plugin_id(&self) -> &'static str {
        "s3"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: S3MountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let filesystem = S3BackedFilesystem::from_config(config)?;
        Ok(Box::new(S3MountedFilesystem::new(filesystem)))
    }
}

struct S3BackedFilesystem {
    inner: MemoryFileSystem,
    store: S3ObjectStore,
    manifest_key: String,
    chunk_key_prefix: String,
    persisted_manifest: PersistedFilesystemManifest,
    chunk_keys: BTreeSet<String>,
    chunk_size: usize,
    inline_threshold: usize,
    dirty_manifest: bool,
    dirty_file_inodes: BTreeSet<u64>,
}

impl S3BackedFilesystem {
    fn from_config(config: S3MountConfig) -> Result<Self, PluginError> {
        let bucket = config.bucket.trim().to_owned();
        if bucket.is_empty() {
            return Err(PluginError::invalid_input(
                "s3 mount requires a non-empty bucket",
            ));
        }

        let chunk_size = config.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);
        if chunk_size == 0 {
            return Err(PluginError::invalid_input(
                "s3 mount requires chunkSize to be greater than zero",
            ));
        }

        let inline_threshold = config.inline_threshold.unwrap_or(DEFAULT_INLINE_THRESHOLD);
        if inline_threshold > chunk_size {
            return Err(PluginError::invalid_input(
                "s3 mount requires inlineThreshold to be less than or equal to chunkSize",
            ));
        }

        let prefix = normalize_prefix(config.prefix.as_deref());
        let manifest_key = format!("{prefix}filesystem-manifest.json");
        let chunk_key_prefix = format!("{prefix}blocks/");
        let store = S3ObjectStore::new(
            bucket,
            config.region.unwrap_or_else(|| DEFAULT_REGION.to_owned()),
            config.endpoint,
            config.credentials,
        )?;

        let (inner, persisted_manifest, chunk_keys) = match store.load_manifest(&manifest_key)? {
            Some(manifest_bytes) => load_filesystem_from_manifest(&store, &manifest_bytes)?,
            None => {
                let inner = MemoryFileSystem::new();
                let manifest = manifest_from_empty_filesystem(&inner);
                (inner, manifest, BTreeSet::new())
            }
        };

        Ok(Self {
            inner,
            store,
            manifest_key,
            chunk_key_prefix,
            persisted_manifest,
            chunk_keys,
            chunk_size,
            inline_threshold,
            dirty_manifest: false,
            dirty_file_inodes: BTreeSet::new(),
        })
    }

    fn flush_pending(&mut self) -> VfsResult<()> {
        if !self.dirty_manifest {
            return Ok(());
        }

        let snapshot = self.inner.snapshot();
        let (manifest, next_chunk_keys) = persist_manifest_from_snapshot(
            &self.store,
            &snapshot,
            &self.persisted_manifest,
            &self.chunk_key_prefix,
            self.chunk_size,
            self.inline_threshold,
            &self.dirty_file_inodes,
        )
        .map_err(storage_error_to_vfs)?;

        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|error| VfsError::io(format!("serialize s3 manifest: {error}")))?;
        self.store
            .put_bytes(&self.manifest_key, &manifest_bytes)
            .map_err(storage_error_to_vfs)?;

        let stale_keys = self
            .chunk_keys
            .difference(&next_chunk_keys)
            .cloned()
            .collect::<Vec<_>>();
        for key in stale_keys {
            self.store
                .delete_object(&key)
                .map_err(storage_error_to_vfs)?;
        }

        self.persisted_manifest = manifest;
        self.chunk_keys = next_chunk_keys;
        self.dirty_manifest = false;
        self.dirty_file_inodes.clear();
        Ok(())
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        self.flush_pending()
    }

    fn mark_manifest_dirty(&mut self) {
        self.dirty_manifest = true;
    }

    fn mark_file_dirty(&mut self, path: &str) -> VfsResult<()> {
        self.dirty_manifest = true;
        let ino = self.inner.lstat(path)?.ino;
        self.dirty_file_inodes.insert(ino);
        Ok(())
    }
}

impl Drop for S3BackedFilesystem {
    fn drop(&mut self) {
        if let Err(error) = self.flush_pending() {
            eprintln!("failed to flush pending S3 filesystem state: {error}");
        }
    }
}

struct S3MountedFilesystem {
    inner: MountedVirtualFileSystem<S3BackedFilesystem>,
}

impl S3MountedFilesystem {
    fn new(inner: S3BackedFilesystem) -> Self {
        Self {
            inner: MountedVirtualFileSystem::new(inner),
        }
    }
}

impl MountedFileSystem for S3MountedFilesystem {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(path)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        self.inner.write_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner.utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        self.inner.inner_mut().shutdown()
    }
}

impl VirtualFileSystem for S3BackedFilesystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(path)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.inner.write_file(path, content.into())?;
        self.mark_file_dirty(path)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(path, recursive)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.rename(old_path, new_path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.symlink(target, link_path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.link(old_path, new_path)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(path, mode)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(path, uid, gid)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner.utimes(path, atime_ms, mtime_ms)?;
        self.mark_manifest_dirty();
        Ok(())
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(path, length)?;
        self.mark_file_dirty(path)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }
}

#[derive(Debug)]
struct S3ObjectStore {
    client: S3Client,
    bucket: String,
}

impl S3ObjectStore {
    fn new(
        bucket: String,
        region: String,
        endpoint: Option<String>,
        credentials: Option<S3MountCredentials>,
    ) -> Result<Self, PluginError> {
        let shared_config = std::thread::spawn(move || -> Result<_, PluginError> {
            let runtime = Runtime::new().map_err(|error| {
                PluginError::unsupported(format!("create tokio runtime: {error}"))
            })?;

            Ok(runtime.block_on(async move {
                let mut loader = aws_config::defaults(BehaviorVersion::latest())
                    .region(aws_sdk_s3::config::Region::new(region));
                if let Some(credentials) = credentials {
                    loader = loader.credentials_provider(Credentials::new(
                        credentials.access_key_id,
                        credentials.secret_access_key,
                        None,
                        None,
                        "agent-os-s3-plugin",
                    ));
                }
                loader.load().await
            }))
        })
        .join()
        .map_err(|_| PluginError::unsupported("s3 runtime thread panicked"))??;

        let mut builder = S3ConfigBuilder::from(&shared_config).force_path_style(true);
        if let Some(endpoint) = endpoint {
            builder = builder.endpoint_url(validate_s3_endpoint(&endpoint)?);
        }

        Ok(Self {
            client: S3Client::from_conf(builder.build()),
            bucket,
        })
    }

    fn load_manifest(&self, key: &str) -> Result<Option<Vec<u8>>, PluginError> {
        self.load_bytes(key)
            .map_err(|error| PluginError::new("EIO", error.to_string()))
    }

    fn load_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let bucket = self.bucket.clone();
        let key = key.to_owned();
        let client = self.client.clone();

        std::thread::spawn(move || -> Result<Option<Vec<u8>>, StorageError> {
            let runtime = Runtime::new()
                .map_err(|error| StorageError::new(format!("create tokio runtime: {error}")))?;

            runtime.block_on(async move {
                match client.get_object().bucket(bucket).key(&key).send().await {
                    Ok(response) => {
                        let bytes = response
                            .body
                            .collect()
                            .await
                            .map_err(|error| {
                                StorageError::new(format!("read s3 object '{key}': {error}"))
                            })?
                            .into_bytes()
                            .to_vec();
                        Ok(Some(bytes))
                    }
                    Err(error) => {
                        if matches!(
                            error.as_service_error().and_then(|service| service.code()),
                            Some("NoSuchKey") | Some("NotFound")
                        ) {
                            return Ok(None);
                        }

                        Err(StorageError::new(format!(
                            "load s3 object '{key}': {error}"
                        )))
                    }
                }
            })
        })
        .join()
        .map_err(|_| StorageError::new("s3 runtime thread panicked"))?
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let bucket = self.bucket.clone();
        let key = key.to_owned();
        let body = bytes.to_vec();
        let client = self.client.clone();

        std::thread::spawn(move || -> Result<(), StorageError> {
            let runtime = Runtime::new()
                .map_err(|error| StorageError::new(format!("create tokio runtime: {error}")))?;

            runtime.block_on(async move {
                client
                    .put_object()
                    .bucket(bucket)
                    .key(&key)
                    .body(ByteStream::from(body))
                    .send()
                    .await
                    .map_err(|error| {
                        StorageError::new(format!("write s3 object '{key}': {error}"))
                    })?;
                Ok(())
            })
        })
        .join()
        .map_err(|_| StorageError::new("s3 runtime thread panicked"))?
    }

    fn delete_object(&self, key: &str) -> Result<(), StorageError> {
        let bucket = self.bucket.clone();
        let key = key.to_owned();
        let client = self.client.clone();

        std::thread::spawn(move || -> Result<(), StorageError> {
            let runtime = Runtime::new()
                .map_err(|error| StorageError::new(format!("create tokio runtime: {error}")))?;

            runtime.block_on(async move {
                match client.delete_object().bucket(bucket).key(&key).send().await {
                    Ok(_) => Ok(()),
                    Err(error)
                        if matches!(
                            error.as_service_error().and_then(|service| service.code()),
                            Some("NoSuchKey") | Some("NotFound")
                        ) =>
                    {
                        Ok(())
                    }
                    Err(error) => Err(StorageError::new(format!(
                        "delete s3 object '{key}': {error}"
                    ))),
                }
            })
        })
        .join()
        .map_err(|_| StorageError::new("s3 runtime thread panicked"))?
    }
}

fn validate_s3_endpoint(raw: &str) -> Result<String, PluginError> {
    let normalized = raw.trim().trim_end_matches('/').to_owned();
    if normalized.is_empty() {
        return Err(PluginError::invalid_input(
            "s3 mount endpoint must be a valid URL",
        ));
    }

    let url = Url::parse(&normalized).map_err(|error| {
        PluginError::invalid_input(format!("s3 mount endpoint is not a valid URL: {error}"))
    })?;
    let host = url
        .host_str()
        .ok_or_else(|| PluginError::invalid_input("s3 mount endpoint must include a host"))?;

    if is_allowed_test_endpoint_host(host) {
        return Ok(normalized);
    }

    if host.eq_ignore_ascii_case("localhost") {
        return Err(PluginError::invalid_input(
            "s3 mount endpoint must not target localhost",
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_disallowed_s3_endpoint_ip(ip) {
            return Err(PluginError::invalid_input(format!(
                "s3 mount endpoint must not target a private or local IP address ({host})"
            )));
        }
    }

    Ok(normalized)
}

fn is_disallowed_s3_endpoint_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.is_unspecified()
        }
    }
}

fn is_allowed_test_endpoint_host(host: &str) -> bool {
    if std::env::var_os("AGENT_OS_ALLOW_LOCAL_S3_ENDPOINTS").is_some() {
        return matches!(host, "127.0.0.1" | "localhost" | "::1");
    }

    #[cfg(test)]
    {
        matches!(host, "127.0.0.1" | "localhost" | "::1")
    }
    #[cfg(not(test))]
    {
        let _ = host;
        false
    }
}

#[derive(Debug, Clone)]
struct StorageError {
    message: String,
}

impl StorageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedFilesystemManifest {
    format: String,
    path_index: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, PersistedFilesystemInode>,
    next_ino: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedFilesystemInode {
    metadata: agent_os_kernel::vfs::MemoryFileSystemSnapshotMetadata,
    kind: PersistedFilesystemInodeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum PersistedFilesystemInodeKind {
    File { storage: PersistedFileStorage },
    Directory,
    SymbolicLink { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "storageMode", rename_all = "camelCase")]
enum PersistedFileStorage {
    Inline {
        data_base64: String,
    },
    Chunked {
        size: u64,
        chunks: Vec<PersistedChunkRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedChunkRef {
    index: u64,
    key: String,
}

fn persist_manifest_from_snapshot(
    store: &S3ObjectStore,
    snapshot: &MemoryFileSystemSnapshot,
    previous_manifest: &PersistedFilesystemManifest,
    chunk_key_prefix: &str,
    chunk_size: usize,
    inline_threshold: usize,
    dirty_file_inodes: &BTreeSet<u64>,
) -> Result<(PersistedFilesystemManifest, BTreeSet<String>), StorageError> {
    let mut chunk_keys = BTreeSet::new();
    let mut inodes = BTreeMap::new();

    for (ino, inode) in &snapshot.inodes {
        let persisted_kind = match &inode.kind {
            MemoryFileSystemSnapshotInodeKind::File { data } => persist_file_inode(
                store,
                *ino,
                data,
                previous_manifest.inodes.get(ino),
                chunk_key_prefix,
                chunk_size,
                inline_threshold,
                dirty_file_inodes.contains(ino),
                &mut chunk_keys,
            )?,
            MemoryFileSystemSnapshotInodeKind::Directory => PersistedFilesystemInodeKind::Directory,
            MemoryFileSystemSnapshotInodeKind::SymbolicLink { target } => {
                PersistedFilesystemInodeKind::SymbolicLink {
                    target: target.clone(),
                }
            }
        };

        inodes.insert(
            *ino,
            PersistedFilesystemInode {
                metadata: inode.metadata.clone(),
                kind: persisted_kind,
            },
        );
    }

    Ok((
        PersistedFilesystemManifest {
            format: MANIFEST_FORMAT.to_owned(),
            path_index: snapshot.path_index.clone(),
            inodes,
            next_ino: snapshot.next_ino,
        },
        chunk_keys,
    ))
}

fn persist_file_inode(
    store: &S3ObjectStore,
    ino: u64,
    data: &[u8],
    previous_inode: Option<&PersistedFilesystemInode>,
    chunk_key_prefix: &str,
    chunk_size: usize,
    inline_threshold: usize,
    data_dirty: bool,
    chunk_keys: &mut BTreeSet<String>,
) -> Result<PersistedFilesystemInodeKind, StorageError> {
    if !data_dirty {
        if let Some(PersistedFilesystemInode {
            kind: PersistedFilesystemInodeKind::File { storage },
            ..
        }) = previous_inode
        {
            collect_chunk_keys_from_storage(storage, chunk_keys);
            return Ok(PersistedFilesystemInodeKind::File {
                storage: storage.clone(),
            });
        }
    }

    let storage = if data.len() <= inline_threshold {
        PersistedFileStorage::Inline {
            data_base64: BASE64.encode(data),
        }
    } else {
        let mut chunks = Vec::new();
        for (index, chunk) in data.chunks(chunk_size).enumerate() {
            let key = format!("{chunk_key_prefix}{ino}/{index}");
            store.put_bytes(&key, chunk)?;
            chunk_keys.insert(key.clone());
            chunks.push(PersistedChunkRef {
                index: index as u64,
                key,
            });
        }

        PersistedFileStorage::Chunked {
            size: data.len() as u64,
            chunks,
        }
    };

    Ok(PersistedFilesystemInodeKind::File { storage })
}

fn collect_chunk_keys_from_storage(
    storage: &PersistedFileStorage,
    chunk_keys: &mut BTreeSet<String>,
) {
    if let PersistedFileStorage::Chunked { chunks, .. } = storage {
        chunk_keys.extend(chunks.iter().map(|chunk| chunk.key.clone()));
    }
}

fn manifest_from_empty_filesystem(inner: &MemoryFileSystem) -> PersistedFilesystemManifest {
    let snapshot = inner.snapshot();
    let root = snapshot
        .inodes
        .get(&1)
        .expect("new memory filesystem should contain root inode");
    PersistedFilesystemManifest {
        format: String::from(MANIFEST_FORMAT),
        path_index: snapshot.path_index,
        inodes: BTreeMap::from([(
            1,
            PersistedFilesystemInode {
                metadata: root.metadata.clone(),
                kind: PersistedFilesystemInodeKind::Directory,
            },
        )]),
        next_ino: snapshot.next_ino,
    }
}

fn load_filesystem_from_manifest(
    store: &S3ObjectStore,
    manifest_bytes: &[u8],
) -> Result<
    (
        MemoryFileSystem,
        PersistedFilesystemManifest,
        BTreeSet<String>,
    ),
    PluginError,
> {
    let manifest: PersistedFilesystemManifest = serde_json::from_slice(manifest_bytes)
        .map_err(|error| PluginError::invalid_input(format!("parse s3 manifest: {error}")))?;
    if manifest.format != MANIFEST_FORMAT {
        return Err(PluginError::invalid_input(format!(
            "unsupported s3 manifest format: {}",
            manifest.format
        )));
    }

    let persisted_manifest = manifest.clone();
    let mut chunk_keys = BTreeSet::new();
    let mut inodes = BTreeMap::new();
    for (ino, inode) in manifest.inodes {
        let kind = match inode.kind {
            PersistedFilesystemInodeKind::File { storage } => {
                let data = match storage {
                    PersistedFileStorage::Inline { data_base64 } => {
                        BASE64.decode(data_base64).map_err(|error| {
                            PluginError::invalid_input(format!(
                                "decode inline s3 file data for inode {ino}: {error}"
                            ))
                        })?
                    }
                    PersistedFileStorage::Chunked { size, mut chunks } => {
                        chunks.sort_by_key(|chunk| chunk.index);
                        let expected_size = validate_manifest_file_size(size, "s3", ino)?;
                        let mut data = Vec::with_capacity(expected_size);
                        for chunk in chunks {
                            let bytes = store
                                .load_bytes(&chunk.key)
                                .map_err(|error| PluginError::new("EIO", error.to_string()))?
                                .ok_or_else(|| {
                                    PluginError::new(
                                        "EIO",
                                        format!(
                                            "s3 manifest references missing chunk '{}' for inode {}",
                                            chunk.key, ino
                                        ),
                                    )
                                })?;
                            chunk_keys.insert(chunk.key);
                            data.extend_from_slice(&bytes);
                        }
                        data.truncate(expected_size);
                        data
                    }
                };

                MemoryFileSystemSnapshotInodeKind::File { data }
            }
            PersistedFilesystemInodeKind::Directory => MemoryFileSystemSnapshotInodeKind::Directory,
            PersistedFilesystemInodeKind::SymbolicLink { target } => {
                MemoryFileSystemSnapshotInodeKind::SymbolicLink { target }
            }
        };

        inodes.insert(
            ino,
            MemoryFileSystemSnapshotInode {
                metadata: inode.metadata,
                kind,
            },
        );
    }

    Ok((
        MemoryFileSystem::from_snapshot(MemoryFileSystemSnapshot {
            path_index: persisted_manifest.path_index.clone(),
            inodes,
            next_ino: persisted_manifest.next_ino,
        }),
        persisted_manifest,
        chunk_keys,
    ))
}

fn validate_manifest_file_size(size: u64, backend: &str, ino: u64) -> Result<usize, PluginError> {
    if size > MAX_PERSISTED_MANIFEST_FILE_BYTES {
        return Err(PluginError::invalid_input(format!(
            "{backend} manifest inode {ino} declares {size} bytes, limit is {MAX_PERSISTED_MANIFEST_FILE_BYTES}"
        )));
    }

    usize::try_from(size).map_err(|_| {
        PluginError::invalid_input(format!(
            "{backend} manifest inode {ino} size {size} does not fit on this platform"
        ))
    })
}

fn normalize_prefix(raw: Option<&str>) -> String {
    match raw {
        Some(prefix) if !prefix.trim().is_empty() => {
            let trimmed = prefix.trim_matches('/');
            if trimmed.is_empty() {
                String::new()
            } else {
                format!("{trimmed}/")
            }
        }
        _ => String::new(),
    }
}

fn storage_error_to_vfs(error: StorageError) -> VfsError {
    VfsError::io(error.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    #[derive(Clone, Debug)]
    pub(crate) struct LoggedRequest {
        pub method: String,
        pub path: String,
    }

    pub(crate) struct MockS3Server {
        base_url: String,
        shutdown: Arc<AtomicBool>,
        objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        requests: Arc<Mutex<Vec<LoggedRequest>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl MockS3Server {
        pub(crate) fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock s3");
            listener
                .set_nonblocking(true)
                .expect("configure mock s3 listener");
            let address = listener.local_addr().expect("resolve mock s3 address");
            let shutdown = Arc::new(AtomicBool::new(false));
            let objects = Arc::new(Mutex::new(BTreeMap::new()));
            let requests = Arc::new(Mutex::new(Vec::new()));
            let shutdown_for_thread = Arc::clone(&shutdown);
            let objects_for_thread = Arc::clone(&objects);
            let requests_for_thread = Arc::clone(&requests);

            let handle = thread::spawn(move || {
                while !shutdown_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            handle_stream(stream, &objects_for_thread, &requests_for_thread);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                base_url: format!("http://{}", address),
                shutdown,
                objects,
                requests,
                handle: Some(handle),
            }
        }

        pub(crate) fn base_url(&self) -> &str {
            &self.base_url
        }

        pub(crate) fn object_keys(&self) -> Vec<String> {
            self.objects
                .lock()
                .expect("lock mock s3 objects")
                .keys()
                .cloned()
                .collect()
        }

        pub(crate) fn put_object(&self, key: &str, bytes: Vec<u8>) {
            self.objects
                .lock()
                .expect("lock mock s3 objects")
                .insert(key.to_owned(), bytes);
        }

        pub(crate) fn requests(&self) -> Vec<LoggedRequest> {
            self.requests.lock().expect("lock mock s3 requests").clone()
        }

        pub(crate) fn clear_requests(&self) {
            self.requests.lock().expect("lock mock s3 requests").clear();
        }
    }

    impl Drop for MockS3Server {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join mock s3 thread");
            }
        }
    }

    fn handle_stream(
        mut stream: TcpStream,
        objects: &Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        requests: &Arc<Mutex<Vec<LoggedRequest>>>,
    ) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set mock s3 read timeout");

        let mut buffer = Vec::new();
        let mut header_end = None;
        while header_end.is_none() {
            let mut chunk = [0; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    header_end = find_header_end(&buffer);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return,
            }
        }

        let header_end = header_end.expect("parse mock s3 headers");
        let header_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = header_text.split("\r\n");
        let request_line = match lines.next() {
            Some(line) if !line.is_empty() => line,
            _ => return,
        };
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default().to_owned();
        let raw_target = request_parts.next().unwrap_or_default();
        let path = decode_path(raw_target.split('?').next().unwrap_or_default());

        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("content-length") {
                    content_length = value.trim().parse::<usize>().unwrap_or(0);
                }
            }
        }

        while buffer.len() < header_end + 4 + content_length {
            let mut chunk = [0; 1024];
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => break,
            }
        }
        let body = buffer[header_end + 4..header_end + 4 + content_length].to_vec();

        requests
            .lock()
            .expect("lock mock s3 request log")
            .push(LoggedRequest {
                method: method.clone(),
                path: path.clone(),
            });

        match method.as_str() {
            "GET" => {
                if let Some(bytes) = objects
                    .lock()
                    .expect("lock mock s3 objects")
                    .get(path.trim_start_matches('/'))
                    .cloned()
                {
                    send_response(&mut stream, 200, "OK", "application/octet-stream", &bytes);
                } else {
                    send_response(
                        &mut stream,
                        404,
                        "Not Found",
                        "application/xml",
                        br#"<Error><Code>NoSuchKey</Code><Message>missing</Message></Error>"#,
                    );
                }
            }
            "PUT" => {
                objects
                    .lock()
                    .expect("lock mock s3 objects")
                    .insert(path.trim_start_matches('/').to_owned(), body);
                send_response(&mut stream, 200, "OK", "application/xml", b"");
            }
            "DELETE" => {
                objects
                    .lock()
                    .expect("lock mock s3 objects")
                    .remove(path.trim_start_matches('/'));
                send_response(&mut stream, 204, "No Content", "application/xml", b"");
            }
            _ => send_response(
                &mut stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"unsupported",
            ),
        }
    }

    fn send_response(
        stream: &mut TcpStream,
        status: u16,
        reason: &str,
        content_type: &str,
        body: &[u8],
    ) {
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\nx-amz-request-id: test\r\n\r\n",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write mock s3 response headers");
        stream.write_all(body).expect("write mock s3 response body");
        stream.flush().expect("flush mock s3 response");
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn decode_path(raw: &str) -> String {
        let mut decoded = String::new();
        let bytes = raw.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' && index + 2 < bytes.len() {
                let code = std::str::from_utf8(&bytes[index + 1..index + 3])
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok());
                if let Some(code) = code {
                    decoded.push(code as char);
                    index += 3;
                    continue;
                }
            }
            if bytes[index] == b'+' {
                decoded.push(' ');
            } else {
                decoded.push(bytes[index] as char);
            }
            index += 1;
        }
        decoded
    }
}
