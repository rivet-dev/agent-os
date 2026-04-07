use agent_os_kernel::mount_plugin::{
    FileSystemPluginFactory, OpenFileSystemPluginRequest, PluginError,
};
use agent_os_kernel::mount_table::{MountedFileSystem, MountedVirtualFileSystem};
use agent_os_kernel::vfs::{
    MemoryFileSystem, MemoryFileSystemSnapshot, MemoryFileSystemSnapshotInode,
    MemoryFileSystemSnapshotInodeKind, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem,
    VirtualStat,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const DEFAULT_CHUNK_SIZE: usize = 4 * 1024 * 1024;
const DEFAULT_INLINE_THRESHOLD: usize = 64 * 1024;
const MANIFEST_FORMAT: &str = "agent_os_google_drive_filesystem_manifest_v1";
const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
const DEFAULT_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const DEFAULT_API_BASE_URL: &str = "https://www.googleapis.com";
const GOOGLE_TOKEN_HOSTS: &[&str] = &["oauth2.googleapis.com"];
const GOOGLE_API_BASE_HOSTS: &[&str] = &["www.googleapis.com"];
const TOKEN_REFRESH_SKEW_SECONDS: u64 = 60;
const MAX_PERSISTED_MANIFEST_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleDriveMountCredentials {
    client_email: String,
    private_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleDriveMountConfig {
    credentials: GoogleDriveMountCredentials,
    folder_id: String,
    key_prefix: Option<String>,
    chunk_size: Option<usize>,
    inline_threshold: Option<usize>,
    #[serde(default)]
    token_url: Option<String>,
    #[serde(default)]
    api_base_url: Option<String>,
}

#[derive(Debug)]
pub(crate) struct GoogleDriveMountPlugin;

impl<Context> FileSystemPluginFactory<Context> for GoogleDriveMountPlugin {
    fn plugin_id(&self) -> &'static str {
        "google_drive"
    }

    fn open(
        &self,
        request: OpenFileSystemPluginRequest<'_, Context>,
    ) -> Result<Box<dyn MountedFileSystem>, PluginError> {
        let config: GoogleDriveMountConfig = serde_json::from_value(request.config.clone())
            .map_err(|error| PluginError::invalid_input(error.to_string()))?;
        let filesystem = GoogleDriveBackedFilesystem::from_config(config)?;
        Ok(Box::new(MountedVirtualFileSystem::new(filesystem)))
    }
}

struct GoogleDriveBackedFilesystem {
    inner: MemoryFileSystem,
    store: GoogleDriveObjectStore,
    manifest_key: String,
    chunk_key_prefix: String,
    chunk_keys: BTreeSet<String>,
    chunk_size: usize,
    inline_threshold: usize,
}

impl GoogleDriveBackedFilesystem {
    fn from_config(config: GoogleDriveMountConfig) -> Result<Self, PluginError> {
        let folder_id = config.folder_id.trim().to_owned();
        if folder_id.is_empty() {
            return Err(PluginError::invalid_input(
                "google_drive mount requires a non-empty folderId",
            ));
        }

        let chunk_size = config.chunk_size.unwrap_or(DEFAULT_CHUNK_SIZE);
        if chunk_size == 0 {
            return Err(PluginError::invalid_input(
                "google_drive mount requires chunkSize to be greater than zero",
            ));
        }

        let inline_threshold = config.inline_threshold.unwrap_or(DEFAULT_INLINE_THRESHOLD);
        if inline_threshold > chunk_size {
            return Err(PluginError::invalid_input(
                "google_drive mount requires inlineThreshold to be less than or equal to chunkSize",
            ));
        }

        let prefix = normalize_prefix(config.key_prefix.as_deref());
        let manifest_key = format!("{prefix}filesystem-manifest.json");
        let chunk_key_prefix = format!("{prefix}blocks/");
        let mut store = GoogleDriveObjectStore::new(
            config.credentials,
            folder_id,
            config
                .token_url
                .unwrap_or_else(|| String::from(DEFAULT_TOKEN_URL)),
            config
                .api_base_url
                .unwrap_or_else(|| String::from(DEFAULT_API_BASE_URL)),
        )?;

        let (inner, chunk_keys) = match store.load_manifest(&manifest_key)? {
            Some(manifest_bytes) => load_filesystem_from_manifest(&mut store, &manifest_bytes)?,
            None => (MemoryFileSystem::new(), BTreeSet::new()),
        };

        Ok(Self {
            inner,
            store,
            manifest_key,
            chunk_key_prefix,
            chunk_keys,
            chunk_size,
            inline_threshold,
        })
    }

    fn persist(&mut self) -> VfsResult<()> {
        let snapshot = self.inner.snapshot();
        let (manifest, next_chunk_keys) = persist_manifest_from_snapshot(
            &mut self.store,
            &snapshot,
            &self.chunk_key_prefix,
            self.chunk_size,
            self.inline_threshold,
        )
        .map_err(storage_error_to_vfs)?;

        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|error| VfsError::io(format!("serialize google drive manifest: {error}")))?;
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

        self.chunk_keys = next_chunk_keys;
        Ok(())
    }
}

impl VirtualFileSystem for GoogleDriveBackedFilesystem {
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
        self.persist()
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.create_dir(path)?;
        self.persist()
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.inner.mkdir(path, recursive)?;
        self.persist()
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_file(path)?;
        self.persist()
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        self.inner.remove_dir(path)?;
        self.persist()
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.rename(old_path, new_path)?;
        self.persist()
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.symlink(target, link_path)?;
        self.persist()
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.inner.link(old_path, new_path)?;
        self.persist()
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        self.inner.chmod(path, mode)?;
        self.persist()
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.inner.chown(path, uid, gid)?;
        self.persist()
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.inner.utimes(path, atime_ms, mtime_ms)?;
        self.persist()
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        self.inner.truncate(path, length)?;
        self.persist()
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }
}

struct GoogleDriveObjectStore {
    auth: GoogleServiceAccountAuth,
    folder_id: String,
    api_base_url: String,
    file_id_cache: BTreeMap<String, String>,
}

impl GoogleDriveObjectStore {
    fn new(
        credentials: GoogleDriveMountCredentials,
        folder_id: String,
        token_url: String,
        api_base_url: String,
    ) -> Result<Self, PluginError> {
        let api_base_url =
            validate_google_drive_url(&api_base_url, "apiBaseUrl", GOOGLE_API_BASE_HOSTS, false)?;

        Ok(Self {
            auth: GoogleServiceAccountAuth::new(credentials, token_url)?,
            folder_id,
            api_base_url,
            file_id_cache: BTreeMap::new(),
        })
    }

    fn load_manifest(&mut self, key: &str) -> Result<Option<Vec<u8>>, PluginError> {
        self.load_bytes(key)
            .map_err(|error| PluginError::new("EIO", error.to_string()))
    }

    fn load_bytes(&mut self, key: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(file_id) = self.find_file_id(key)? else {
            return Ok(None);
        };

        match self.download_file(&file_id) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.is_not_found() => {
                self.file_id_cache.remove(key);
                if let Some(file_id) = self.lookup_file_id(key)? {
                    let bytes = self.download_file(&file_id)?;
                    Ok(Some(bytes))
                } else {
                    Ok(None)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn put_bytes(&mut self, key: &str, bytes: &[u8]) -> Result<(), StorageError> {
        if let Some(file_id) = self.find_file_id(key)? {
            match self.upload_file_contents(&file_id, bytes) {
                Ok(()) => return Ok(()),
                Err(error) if error.is_not_found() => {
                    self.file_id_cache.remove(key);
                }
                Err(error) => return Err(error),
            }
        }

        let file_id = self.create_file(key)?;
        self.upload_file_contents(&file_id, bytes)?;
        self.file_id_cache.insert(String::from(key), file_id);
        Ok(())
    }

    fn delete_object(&mut self, key: &str) -> Result<(), StorageError> {
        let Some(file_id) = self.find_file_id(key)? else {
            return Ok(());
        };

        match self.delete_file(&file_id) {
            Ok(()) => {}
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error),
        }

        self.file_id_cache.remove(key);
        Ok(())
    }

    fn find_file_id(&mut self, key: &str) -> Result<Option<String>, StorageError> {
        if let Some(file_id) = self.file_id_cache.get(key) {
            return Ok(Some(file_id.clone()));
        }

        let file_id = self.lookup_file_id(key)?;
        if let Some(file_id) = &file_id {
            self.file_id_cache
                .insert(String::from(key), file_id.clone());
        }
        Ok(file_id)
    }

    fn lookup_file_id(&mut self, key: &str) -> Result<Option<String>, StorageError> {
        let query = format!(
            "name = '{}' and '{}' in parents and trashed = false",
            escape_query_literal(key),
            escape_query_literal(&self.folder_id),
        );
        let token = self.auth.access_token()?;
        let url = format!("{}/drive/v3/files", self.api_base_url);

        match ureq::get(&url)
            .query("q", &query)
            .query("fields", "files(id)")
            .query("pageSize", "1")
            .query("supportsAllDrives", "true")
            .set("Authorization", &format!("Bearer {token}"))
            .call()
        {
            Ok(response) => {
                let payload = response
                    .into_json::<DriveFileListResponse>()
                    .map_err(|error| {
                        StorageError::new(format!(
                            "decode google drive file lookup response: {error}"
                        ))
                    })?;
                Ok(payload
                    .files
                    .and_then(|mut files| files.pop())
                    .and_then(|file| file.id))
            }
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                &format!("lookup google drive file '{key}'"),
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "lookup google drive file '{key}': {error}"
            ))),
        }
    }

    fn download_file(&mut self, file_id: &str) -> Result<Vec<u8>, StorageError> {
        let token = self.auth.access_token()?;
        let url = format!("{}/drive/v3/files/{}", self.api_base_url, file_id);

        match ureq::get(&url)
            .query("alt", "media")
            .query("supportsAllDrives", "true")
            .set("Authorization", &format!("Bearer {token}"))
            .call()
        {
            Ok(response) => read_response_bytes(response).map_err(|error| {
                StorageError::new(format!("read google drive file '{file_id}': {error}"))
            }),
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                &format!("download google drive file '{file_id}'"),
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "download google drive file '{file_id}': {error}"
            ))),
        }
    }

    fn create_file(&mut self, name: &str) -> Result<String, StorageError> {
        let token = self.auth.access_token()?;
        let url = format!("{}/drive/v3/files", self.api_base_url);

        match ureq::post(&url)
            .query("fields", "id")
            .query("supportsAllDrives", "true")
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(json!({
                "name": name,
                "parents": [self.folder_id.clone()],
                "mimeType": "application/octet-stream",
            })) {
            Ok(response) => {
                let payload = response.into_json::<DriveFileResponse>().map_err(|error| {
                    StorageError::new(format!("decode google drive file create response: {error}"))
                })?;
                payload.id.ok_or_else(|| {
                    StorageError::new(format!(
                        "create google drive file '{name}': missing file id in response"
                    ))
                })
            }
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                &format!("create google drive file '{name}'"),
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "create google drive file '{name}': {error}"
            ))),
        }
    }

    fn upload_file_contents(&mut self, file_id: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let token = self.auth.access_token()?;
        let url = format!("{}/upload/drive/v3/files/{}", self.api_base_url, file_id);

        match ureq::request("PATCH", &url)
            .query("uploadType", "media")
            .query("supportsAllDrives", "true")
            .set("Authorization", &format!("Bearer {token}"))
            .set("Content-Type", "application/octet-stream")
            .send_bytes(bytes)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                &format!("upload google drive file '{file_id}'"),
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "upload google drive file '{file_id}': {error}"
            ))),
        }
    }

    fn delete_file(&mut self, file_id: &str) -> Result<(), StorageError> {
        let token = self.auth.access_token()?;
        let url = format!("{}/drive/v3/files/{}", self.api_base_url, file_id);

        match ureq::delete(&url)
            .query("supportsAllDrives", "true")
            .set("Authorization", &format!("Bearer {token}"))
            .call()
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                &format!("delete google drive file '{file_id}'"),
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "delete google drive file '{file_id}': {error}"
            ))),
        }
    }
}

struct GoogleServiceAccountAuth {
    client_email: String,
    token_url: String,
    encoding_key: EncodingKey,
    cached_token: Option<CachedAccessToken>,
}

#[derive(Debug, Clone)]
struct CachedAccessToken {
    access_token: String,
    expires_at: u64,
}

impl GoogleServiceAccountAuth {
    fn new(
        credentials: GoogleDriveMountCredentials,
        token_url: String,
    ) -> Result<Self, PluginError> {
        if credentials.client_email.trim().is_empty() {
            return Err(PluginError::invalid_input(
                "google_drive mount requires credentials.clientEmail",
            ));
        }
        if credentials.private_key.trim().is_empty() {
            return Err(PluginError::invalid_input(
                "google_drive mount requires credentials.privateKey",
            ));
        }
        let encoding_key =
            EncodingKey::from_rsa_pem(credentials.private_key.as_bytes()).map_err(|error| {
                PluginError::invalid_input(format!(
                    "google_drive mount credentials.privateKey is not valid PEM: {error}"
                ))
            })?;
        let token_url =
            validate_google_drive_url(&token_url, "tokenUrl", GOOGLE_TOKEN_HOSTS, true)?;

        Ok(Self {
            client_email: credentials.client_email,
            token_url,
            encoding_key,
            cached_token: None,
        })
    }

    fn access_token(&mut self) -> Result<String, StorageError> {
        let now = now_unix_seconds();
        if let Some(token) = &self.cached_token {
            if token.expires_at > now + TOKEN_REFRESH_SKEW_SECONDS {
                return Ok(token.access_token.clone());
            }
        }

        let iat = now as usize;
        let exp = (now + 3600) as usize;
        let claims = ServiceAccountClaims {
            iss: &self.client_email,
            scope: DRIVE_SCOPE,
            aud: &self.token_url,
            iat,
            exp,
        };
        let jwt = jsonwebtoken::encode(&Header::new(Algorithm::RS256), &claims, &self.encoding_key)
            .map_err(|error| StorageError::new(format!("sign google oauth assertion: {error}")))?;

        match ureq::post(&self.token_url).send_form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", jwt.as_str()),
        ]) {
            Ok(response) => {
                let payload = response
                    .into_json::<AccessTokenResponse>()
                    .map_err(|error| {
                        StorageError::new(format!("decode google oauth token response: {error}"))
                    })?;
                let cached = CachedAccessToken {
                    access_token: payload.access_token,
                    expires_at: now + payload.expires_in,
                };
                let token = cached.access_token.clone();
                self.cached_token = Some(cached);
                Ok(token)
            }
            Err(ureq::Error::Status(status, response)) => Err(response_error(
                "fetch google oauth access token",
                status,
                response,
            )),
            Err(ureq::Error::Transport(error)) => Err(StorageError::new(format!(
                "fetch google oauth access token: {error}"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: String,
    expires_in: u64,
}

#[derive(Debug, Serialize)]
struct ServiceAccountClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: usize,
    exp: usize,
}

#[derive(Debug, Deserialize)]
struct DriveFileListResponse {
    files: Option<Vec<DriveFileResponse>>,
}

#[derive(Debug, Deserialize)]
struct DriveFileResponse {
    id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageErrorKind {
    Other,
    NotFound,
}

#[derive(Debug, Clone)]
struct StorageError {
    kind: StorageErrorKind,
    message: String,
}

impl StorageError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            kind: StorageErrorKind::Other,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: StorageErrorKind::NotFound,
            message: message.into(),
        }
    }

    fn is_not_found(&self) -> bool {
        self.kind == StorageErrorKind::NotFound
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StorageError {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedFilesystemManifest {
    format: String,
    path_index: BTreeMap<String, u64>,
    inodes: BTreeMap<u64, PersistedFilesystemInode>,
    next_ino: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedFilesystemInode {
    metadata: agent_os_kernel::vfs::MemoryFileSystemSnapshotMetadata,
    kind: PersistedFilesystemInodeKind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum PersistedFilesystemInodeKind {
    File { storage: PersistedFileStorage },
    Directory,
    SymbolicLink { target: String },
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedChunkRef {
    index: u64,
    key: String,
}

fn persist_manifest_from_snapshot(
    store: &mut GoogleDriveObjectStore,
    snapshot: &MemoryFileSystemSnapshot,
    chunk_key_prefix: &str,
    chunk_size: usize,
    inline_threshold: usize,
) -> Result<(PersistedFilesystemManifest, BTreeSet<String>), StorageError> {
    let mut chunk_keys = BTreeSet::new();
    let mut inodes = BTreeMap::new();

    for (ino, inode) in &snapshot.inodes {
        let persisted_kind = match &inode.kind {
            MemoryFileSystemSnapshotInodeKind::File { data } => {
                if data.len() <= inline_threshold {
                    PersistedFilesystemInodeKind::File {
                        storage: PersistedFileStorage::Inline {
                            data_base64: BASE64.encode(data),
                        },
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

                    PersistedFilesystemInodeKind::File {
                        storage: PersistedFileStorage::Chunked {
                            size: data.len() as u64,
                            chunks,
                        },
                    }
                }
            }
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
            format: String::from(MANIFEST_FORMAT),
            path_index: snapshot.path_index.clone(),
            inodes,
            next_ino: snapshot.next_ino,
        },
        chunk_keys,
    ))
}

fn load_filesystem_from_manifest(
    store: &mut GoogleDriveObjectStore,
    manifest_bytes: &[u8],
) -> Result<(MemoryFileSystem, BTreeSet<String>), PluginError> {
    let manifest: PersistedFilesystemManifest =
        serde_json::from_slice(manifest_bytes).map_err(|error| {
            PluginError::invalid_input(format!("parse google drive manifest: {error}"))
        })?;
    if manifest.format != MANIFEST_FORMAT {
        return Err(PluginError::invalid_input(format!(
            "unsupported google drive manifest format: {}",
            manifest.format
        )));
    }

    let mut chunk_keys = BTreeSet::new();
    let mut inodes = BTreeMap::new();
    for (ino, inode) in manifest.inodes {
        let kind = match inode.kind {
            PersistedFilesystemInodeKind::File { storage } => {
                let data = match storage {
                    PersistedFileStorage::Inline { data_base64 } => {
                        BASE64.decode(data_base64).map_err(|error| {
                            PluginError::invalid_input(format!(
                                "decode inline google drive file data for inode {ino}: {error}"
                            ))
                        })?
                    }
                    PersistedFileStorage::Chunked { size, mut chunks } => {
                        chunks.sort_by_key(|chunk| chunk.index);
                        let expected_size = validate_manifest_file_size(size, "google drive", ino)?;
                        let mut data = Vec::with_capacity(expected_size);
                        for chunk in chunks {
                            let bytes = store
                                .load_bytes(&chunk.key)
                                .map_err(|error| PluginError::new("EIO", error.to_string()))?
                                .ok_or_else(|| {
                                    PluginError::new(
                                        "EIO",
                                        format!(
                                            "google drive manifest references missing chunk '{}' for inode {}",
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
            path_index: manifest.path_index,
            inodes,
            next_ino: manifest.next_ino,
        }),
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

fn normalize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(String::from(trimmed))
    }
}

fn validate_google_drive_url(
    raw: &str,
    field_name: &str,
    allowed_hosts: &[&str],
    allow_path: bool,
) -> Result<String, PluginError> {
    let normalized = normalize_base_url(raw).ok_or_else(|| {
        PluginError::invalid_input(format!("google_drive mount requires a valid {field_name}"))
    })?;
    let url = Url::parse(&normalized).map_err(|error| {
        PluginError::invalid_input(format!(
            "google_drive mount {field_name} is not a valid URL: {error}"
        ))
    })?;

    if is_google_drive_test_url(&url) {
        return Ok(normalized);
    }

    if url.scheme() != "https" {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must use https"
        )));
    }
    if url.host_str().is_none() {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must include a host"
        )));
    }
    if url.port().is_some() {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must not override the default port"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must not include user credentials"
        )));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must not include query or fragment components"
        )));
    }
    if !allow_path && url.path() != "/" {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} must not include a path"
        )));
    }

    let host = url.host_str().expect("host checked above");
    if !allowed_hosts.iter().any(|candidate| candidate == &host) {
        return Err(PluginError::invalid_input(format!(
            "google_drive mount {field_name} host must be one of: {}",
            allowed_hosts.join(", ")
        )));
    }

    Ok(normalized)
}

fn is_google_drive_test_url(url: &Url) -> bool {
    #[cfg(test)]
    {
        matches!(url.scheme(), "http" | "https")
            && matches!(
                url.host_str(),
                Some("127.0.0.1") | Some("localhost") | Some("[::1]")
            )
    }
    #[cfg(not(test))]
    {
        let _ = url;
        false
    }
}

fn escape_query_literal(raw: &str) -> String {
    raw.replace('\'', "\\'")
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_secs()
}

fn read_response_bytes(response: ureq::Response) -> std::io::Result<Vec<u8>> {
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn response_error(context: &str, status: u16, response: ureq::Response) -> StorageError {
    let body = response.into_string().unwrap_or_default();
    let message = if body.trim().is_empty() {
        format!("{context}: http {status}")
    } else {
        format!("{context}: http {status}: {}", body.trim())
    };
    if status == 404 {
        StorageError::not_found(message)
    } else {
        StorageError::new(message)
    }
}

fn storage_error_to_vfs(error: StorageError) -> VfsError {
    VfsError::io(error.to_string())
}

#[cfg(test)]
pub(crate) mod test_support {
    use serde::Deserialize;
    use serde_json::json;
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

    #[derive(Clone, Debug)]
    struct MockDriveFile {
        id: String,
        name: String,
        parents: Vec<String>,
        content: Vec<u8>,
    }

    #[derive(Default)]
    struct ServerState {
        next_id: usize,
        files: BTreeMap<String, MockDriveFile>,
        requests: Vec<LoggedRequest>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CreateFileBody {
        name: String,
        parents: Option<Vec<String>>,
    }

    pub(crate) struct MockGoogleDriveServer {
        base_url: String,
        shutdown: Arc<AtomicBool>,
        state: Arc<Mutex<ServerState>>,
        handle: Option<JoinHandle<()>>,
    }

    impl MockGoogleDriveServer {
        pub(crate) fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock google drive");
            listener
                .set_nonblocking(true)
                .expect("configure mock google drive listener");
            let address = listener
                .local_addr()
                .expect("resolve mock google drive address");
            let shutdown = Arc::new(AtomicBool::new(false));
            let state = Arc::new(Mutex::new(ServerState::default()));
            let shutdown_for_thread = Arc::clone(&shutdown);
            let state_for_thread = Arc::clone(&state);

            let handle = thread::spawn(move || {
                while !shutdown_for_thread.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            handle_stream(stream, &state_for_thread);
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
                state,
                handle: Some(handle),
            }
        }

        pub(crate) fn base_url(&self) -> &str {
            &self.base_url
        }

        pub(crate) fn file_names(&self) -> Vec<String> {
            self.state
                .lock()
                .expect("lock mock google drive state")
                .files
                .values()
                .map(|file| file.name.clone())
                .collect()
        }

        pub(crate) fn insert_file(&self, name: &str, parent: &str, content: Vec<u8>) {
            let mut state = self.state.lock().expect("lock mock google drive state");
            state.next_id += 1;
            let file_id = format!("file-{}", state.next_id);
            state.files.insert(
                file_id.clone(),
                MockDriveFile {
                    id: file_id,
                    name: name.to_owned(),
                    parents: vec![parent.to_owned()],
                    content,
                },
            );
        }

        pub(crate) fn requests(&self) -> Vec<LoggedRequest> {
            self.state
                .lock()
                .expect("lock mock google drive state")
                .requests
                .clone()
        }
    }

    impl Drop for MockGoogleDriveServer {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                handle.join().expect("join mock google drive thread");
            }
        }
    }

    fn handle_stream(mut stream: TcpStream, state: &Arc<Mutex<ServerState>>) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set mock google drive read timeout");

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

        let header_end = header_end.expect("parse mock google drive headers");
        let header_text = String::from_utf8_lossy(&buffer[..header_end]);
        let mut lines = header_text.split("\r\n");
        let request_line = match lines.next() {
            Some(line) if !line.is_empty() => line,
            _ => return,
        };
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default().to_owned();
        let raw_target = request_parts.next().unwrap_or_default();
        let (raw_path, raw_query) = raw_target.split_once('?').unwrap_or((raw_target, ""));
        let path = decode_component(raw_path);
        let query = parse_query(raw_query);

        let mut headers = BTreeMap::new();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((name, value)) = line.split_once(':') {
                let header_name = name.trim().to_ascii_lowercase();
                let header_value = value.trim().to_owned();
                if header_name == "content-length" {
                    content_length = header_value.parse::<usize>().unwrap_or(0);
                }
                headers.insert(header_name, header_value);
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

        state
            .lock()
            .expect("lock mock google drive state")
            .requests
            .push(LoggedRequest {
                method: method.clone(),
                path: path.clone(),
            });

        match (method.as_str(), path.as_str()) {
            ("POST", "/token") => send_json_response(
                &mut stream,
                200,
                json!({
                    "access_token": "test-access-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                }),
            ),
            ("GET", "/drive/v3/files") => handle_list(&mut stream, state, &query),
            ("POST", "/drive/v3/files") => handle_create(&mut stream, state, &body),
            ("DELETE", file_path) if file_path.starts_with("/drive/v3/files/") => {
                handle_delete(&mut stream, state, file_path)
            }
            ("POST", copy_path)
                if copy_path.starts_with("/drive/v3/files/") && copy_path.ends_with("/copy") =>
            {
                handle_copy(&mut stream, state, copy_path, &body)
            }
            ("GET", file_path)
                if file_path.starts_with("/drive/v3/files/")
                    && query.get("alt").map(String::as_str) == Some("media") =>
            {
                handle_download(&mut stream, state, file_path, headers.get("range"))
            }
            ("PATCH", upload_path) if upload_path.starts_with("/upload/drive/v3/files/") => {
                handle_upload(&mut stream, state, upload_path, body)
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

    fn handle_list(
        stream: &mut TcpStream,
        state: &Arc<Mutex<ServerState>>,
        query: &BTreeMap<String, String>,
    ) {
        let Some(q) = query.get("q") else {
            send_response(stream, 400, "Bad Request", "text/plain", b"missing q");
            return;
        };
        let Some((name, folder_id)) = parse_list_query(q) else {
            send_response(stream, 400, "Bad Request", "text/plain", b"invalid q");
            return;
        };

        let file = state
            .lock()
            .expect("lock mock google drive state")
            .files
            .values()
            .find(|file| {
                file.name == name && file.parents.iter().any(|parent| parent == &folder_id)
            })
            .cloned();

        let response = match file {
            Some(file) => json!({ "files": [{ "id": file.id }] }),
            None => json!({ "files": [] }),
        };
        send_json_response(stream, 200, response);
    }

    fn handle_create(stream: &mut TcpStream, state: &Arc<Mutex<ServerState>>, body: &[u8]) {
        let Ok(request) = serde_json::from_slice::<CreateFileBody>(body) else {
            send_response(stream, 400, "Bad Request", "text/plain", b"invalid json");
            return;
        };
        let mut state = state.lock().expect("lock mock google drive state");
        state.next_id += 1;
        let file_id = format!("file-{}", state.next_id);
        state.files.insert(
            file_id.clone(),
            MockDriveFile {
                id: file_id.clone(),
                name: request.name,
                parents: request.parents.unwrap_or_default(),
                content: Vec::new(),
            },
        );
        send_json_response(stream, 200, json!({ "id": file_id }));
    }

    fn handle_upload(
        stream: &mut TcpStream,
        state: &Arc<Mutex<ServerState>>,
        path: &str,
        body: Vec<u8>,
    ) {
        let file_id = path.trim_start_matches("/upload/drive/v3/files/");
        let mut state = state.lock().expect("lock mock google drive state");
        let Some(file) = state.files.get_mut(file_id) else {
            send_response(
                stream,
                404,
                "Not Found",
                "application/json",
                br#"{"error":"missing"}"#,
            );
            return;
        };
        file.content = body;
        send_json_response(stream, 200, json!({ "id": file.id }));
    }

    fn handle_download(
        stream: &mut TcpStream,
        state: &Arc<Mutex<ServerState>>,
        path: &str,
        range_header: Option<&String>,
    ) {
        let file_id = path.trim_start_matches("/drive/v3/files/");
        let Some(file) = state
            .lock()
            .expect("lock mock google drive state")
            .files
            .get(file_id)
            .cloned()
        else {
            send_response(
                stream,
                404,
                "Not Found",
                "application/json",
                br#"{"error":"missing"}"#,
            );
            return;
        };

        if let Some(range_header) = range_header {
            let Some((start, end)) = parse_byte_range(range_header) else {
                send_response(stream, 400, "Bad Request", "text/plain", b"invalid range");
                return;
            };
            if start >= file.content.len() as u64 {
                send_response(stream, 416, "Range Not Satisfiable", "text/plain", b"");
                return;
            }
            let end = end.min(file.content.len().saturating_sub(1) as u64);
            let body = &file.content[start as usize..=end as usize];
            send_response(
                stream,
                206,
                "Partial Content",
                "application/octet-stream",
                body,
            );
            return;
        }

        send_response(stream, 200, "OK", "application/octet-stream", &file.content);
    }

    fn handle_delete(stream: &mut TcpStream, state: &Arc<Mutex<ServerState>>, path: &str) {
        let file_id = path.trim_start_matches("/drive/v3/files/");
        let removed = state
            .lock()
            .expect("lock mock google drive state")
            .files
            .remove(file_id);
        if removed.is_some() {
            send_response(stream, 204, "No Content", "application/json", b"");
        } else {
            send_response(
                stream,
                404,
                "Not Found",
                "application/json",
                br#"{"error":"missing"}"#,
            );
        }
    }

    fn handle_copy(
        stream: &mut TcpStream,
        state: &Arc<Mutex<ServerState>>,
        path: &str,
        body: &[u8],
    ) {
        let source_id = path
            .trim_start_matches("/drive/v3/files/")
            .trim_end_matches("/copy");
        let Ok(request) = serde_json::from_slice::<CreateFileBody>(body) else {
            send_response(stream, 400, "Bad Request", "text/plain", b"invalid json");
            return;
        };
        let mut state = state.lock().expect("lock mock google drive state");
        let Some(source) = state.files.get(source_id).cloned() else {
            send_response(
                stream,
                404,
                "Not Found",
                "application/json",
                br#"{"error":"missing"}"#,
            );
            return;
        };
        state.next_id += 1;
        let file_id = format!("file-{}", state.next_id);
        state.files.insert(
            file_id.clone(),
            MockDriveFile {
                id: file_id.clone(),
                name: request.name,
                parents: request.parents.unwrap_or_default(),
                content: source.content,
            },
        );
        send_json_response(stream, 200, json!({ "id": file_id }));
    }

    fn send_json_response(stream: &mut TcpStream, status: u16, body: serde_json::Value) {
        let bytes = serde_json::to_vec(&body).expect("serialize mock google drive response");
        let reason = match status {
            200 => "OK",
            204 => "No Content",
            400 => "Bad Request",
            404 => "Not Found",
            _ => "OK",
        };
        send_response(stream, status, reason, "application/json", &bytes);
    }

    fn send_response(
        stream: &mut TcpStream,
        status: u16,
        reason: &str,
        content_type: &str,
        body: &[u8],
    ) {
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write mock google drive response headers");
        stream
            .write_all(body)
            .expect("write mock google drive response body");
        stream.flush().expect("flush mock google drive response");
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn parse_query(raw: &str) -> BTreeMap<String, String> {
        raw.split('&')
            .filter(|pair| !pair.is_empty())
            .filter_map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                Some((decode_component(name), decode_component(value)))
            })
            .collect()
    }

    fn decode_component(raw: &str) -> String {
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

    fn parse_list_query(query: &str) -> Option<(String, String)> {
        let name_prefix = "name = ";
        let name_start = query.find(name_prefix)? + name_prefix.len();
        let (name, cursor) = parse_single_quoted_literal(query, name_start)?;
        let parent_prefix = " and ";
        let parent_start = query[cursor..].find(parent_prefix)? + cursor + parent_prefix.len();
        let (folder_id, _) = parse_single_quoted_literal(query, parent_start)?;
        Some((name, folder_id))
    }

    fn parse_single_quoted_literal(input: &str, start: usize) -> Option<(String, usize)> {
        let bytes = input.as_bytes();
        if bytes.get(start)? != &b'\'' {
            return None;
        }
        let mut index = start + 1;
        let mut decoded = String::new();
        while index < bytes.len() {
            match bytes[index] {
                b'\\' if index + 1 < bytes.len() => {
                    decoded.push(bytes[index + 1] as char);
                    index += 2;
                }
                b'\'' => return Some((decoded, index + 1)),
                byte => {
                    decoded.push(byte as char);
                    index += 1;
                }
            }
        }
        None
    }

    fn parse_byte_range(header: &str) -> Option<(u64, u64)> {
        let value = header.strip_prefix("bytes=")?;
        let (start, end) = value.split_once('-')?;
        Some((start.parse().ok()?, end.parse().ok()?))
    }
}
