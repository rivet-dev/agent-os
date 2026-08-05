use super::block_store::{collect_body, s3_error};
use agentos_vfs_core::engine::{
    InodeType, ObjectBackend, ObjectEntry, ObjectMeta, Timespec, VfsError, VfsResult,
};
use async_trait::async_trait;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct S3ObjectBackendOptions {
    pub prefix: String,
}

#[derive(Debug, Clone)]
pub struct S3ObjectBackend {
    client: Client,
    bucket: String,
    options: S3ObjectBackendOptions,
}

impl S3ObjectBackend {
    pub fn new(client: Client, bucket: impl Into<String>) -> Self {
        Self::with_options(client, bucket, S3ObjectBackendOptions::default())
    }

    pub fn with_options(
        client: Client,
        bucket: impl Into<String>,
        options: S3ObjectBackendOptions,
    ) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            options,
        }
    }

    fn key_for(&self, key: &str) -> String {
        format!("{}{}", self.options.prefix, key)
    }

    fn strip_prefix<'a>(&self, key: &'a str) -> &'a str {
        key.strip_prefix(&self.options.prefix).unwrap_or(key)
    }
}

#[async_trait]
impl ObjectBackend for S3ObjectBackend {
    async fn list(&self, prefix: &str) -> VfsResult<Vec<ObjectEntry>> {
        let s3_prefix = self.key_for(prefix);
        let mut continuation = None;
        let mut entries = Vec::new();
        loop {
            let response = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&s3_prefix)
                .delimiter("/")
                .set_continuation_token(continuation)
                .send()
                .await
                .map_err(|err| s3_error(format!("list s3 prefix '{s3_prefix}': {err}")))?;

            for object in response.contents() {
                let Some(key) = object.key() else {
                    continue;
                };
                entries.push(ObjectEntry {
                    name: self.strip_prefix(key).to_string(),
                    size: object.size().unwrap_or(0).max(0) as u64,
                    mtime: object
                        .last_modified()
                        .map(timespec_from_smithy)
                        .unwrap_or_else(Timespec::now),
                    is_prefix: false,
                });
            }

            for prefix in response.common_prefixes() {
                let Some(prefix) = prefix.prefix() else {
                    continue;
                };
                entries.push(ObjectEntry {
                    name: self.strip_prefix(prefix).to_string(),
                    size: 0,
                    mtime: Timespec::now(),
                    is_prefix: true,
                });
            }

            if response.is_truncated().unwrap_or(false) {
                continuation = response.next_continuation_token().map(ToOwned::to_owned);
            } else {
                break;
            }
        }
        Ok(entries)
    }

    async fn head(&self, key: &str) -> VfsResult<Option<ObjectMeta>> {
        let s3_key = self.key_for(key);
        let response = match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
        {
            Ok(response) => response,
            Err(err)
                if err.as_service_error().is_some_and(|err| {
                    err.is_not_found()
                        || matches!(err.meta().code(), Some("NotFound" | "NoSuchKey"))
                }) =>
            {
                return Ok(None);
            }
            Err(err) => return Err(s3_error(format!("head s3 object '{s3_key}': {err}"))),
        };

        let metadata = response.metadata();
        let kind = metadata
            .and_then(|metadata| metadata.get("vfs-kind"))
            .map(|kind| match kind.as_str() {
                "directory" => InodeType::Directory,
                "symlink" => InodeType::Symlink,
                "character-device" => InodeType::CharacterDevice,
                "block-device" => InodeType::BlockDevice,
                "fifo" => InodeType::Fifo,
                _ => InodeType::File,
            })
            .unwrap_or(InodeType::File);
        let xattrs = metadata
            .and_then(|metadata| metadata.get("vfs-xattrs"))
            .map(|encoded| {
                let bytes = BASE64
                    .decode(encoded)
                    .map_err(|error| VfsError::eio(format!("decode S3 xattrs: {error}")))?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| VfsError::eio(format!("parse S3 xattrs: {error}")))
            })
            .transpose()?
            .unwrap_or_default();
        let size = response.content_length().unwrap_or(0).max(0) as u64;
        let allocated_extents = metadata
            .and_then(|metadata| metadata.get("vfs-allocated-extents"))
            .map(|encoded| {
                let bytes = BASE64.decode(encoded).map_err(|error| {
                    VfsError::eio(format!("decode S3 allocated extents: {error}"))
                })?;
                serde_json::from_slice(&bytes)
                    .map_err(|error| VfsError::eio(format!("parse S3 allocated extents: {error}")))
            })
            .transpose()?
            .unwrap_or_else(|| {
                if size == 0 {
                    Vec::new()
                } else {
                    vec![(0, size.div_ceil(512))]
                }
            });
        let fallback_time = response
            .last_modified()
            .map(timespec_from_smithy)
            .unwrap_or_else(Timespec::now);
        Ok(Some(ObjectMeta {
            size,
            allocated_extents,
            atime: metadata
                .and_then(|metadata| metadata.get("vfs-atime"))
                .and_then(|value| parse_timespec(value))
                .unwrap_or(fallback_time),
            mtime: metadata
                .and_then(|metadata| metadata.get("vfs-mtime"))
                .and_then(|value| parse_timespec(value))
                .unwrap_or(fallback_time),
            ctime: metadata
                .and_then(|metadata| metadata.get("vfs-ctime"))
                .and_then(|value| parse_timespec(value))
                .unwrap_or(fallback_time),
            birthtime: metadata
                .and_then(|metadata| metadata.get("vfs-birthtime"))
                .and_then(|value| parse_timespec(value))
                .unwrap_or(fallback_time),
            mode: metadata
                .and_then(|metadata| metadata.get("vfs-mode"))
                .and_then(|mode| u32::from_str_radix(mode, 8).ok())
                .unwrap_or(0o644),
            uid: metadata
                .and_then(|metadata| metadata.get("vfs-uid"))
                .and_then(|uid| uid.parse().ok())
                .unwrap_or(0),
            gid: metadata
                .and_then(|metadata| metadata.get("vfs-gid"))
                .and_then(|gid| gid.parse().ok())
                .unwrap_or(0),
            kind,
            symlink_target: metadata
                .and_then(|metadata| metadata.get("vfs-symlink-target").cloned()),
            link_id: metadata.and_then(|metadata| metadata.get("vfs-link-id").cloned()),
            xattrs,
        }))
    }

    async fn get_range(&self, key: &str, off: u64, len: u64) -> VfsResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let s3_key = self.key_for(key);
        let end = off
            .checked_add(len)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| agentos_vfs_core::engine::VfsError::einval("invalid S3 byte range"))?;
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .range(format!("bytes={off}-{end}"))
            .send()
            .await
            .map_err(|err| s3_error(format!("get s3 object range '{s3_key}': {err}")))?;
        collect_body(response.body, &s3_key).await
    }

    async fn put(&self, key: &str, data: &[u8], meta: ObjectMeta) -> VfsResult<()> {
        let s3_key = self.key_for(key);
        let mut metadata = HashMap::new();
        metadata.insert("vfs-kind".to_string(), kind_name(meta.kind).to_string());
        metadata.insert("vfs-mode".to_string(), format!("{:o}", meta.mode));
        metadata.insert("vfs-uid".to_string(), meta.uid.to_string());
        metadata.insert("vfs-gid".to_string(), meta.gid.to_string());
        metadata.insert("vfs-atime".to_string(), format_timespec(meta.atime));
        metadata.insert("vfs-mtime".to_string(), format_timespec(meta.mtime));
        metadata.insert("vfs-ctime".to_string(), format_timespec(meta.ctime));
        metadata.insert("vfs-birthtime".to_string(), format_timespec(meta.birthtime));
        let encoded_extents = serde_json::to_vec(&meta.allocated_extents)
            .map(|bytes| BASE64.encode(bytes))
            .map_err(|error| VfsError::eio(format!("serialize S3 allocated extents: {error}")))?;
        metadata.insert("vfs-allocated-extents".to_string(), encoded_extents);
        if let Some(target) = meta.symlink_target {
            metadata.insert("vfs-symlink-target".to_string(), target);
        }
        if let Some(link_id) = meta.link_id {
            metadata.insert("vfs-link-id".to_string(), link_id);
        }
        if !meta.xattrs.is_empty() {
            let encoded = serde_json::to_vec(&meta.xattrs)
                .map(|bytes| BASE64.encode(bytes))
                .map_err(|error| VfsError::eio(format!("serialize S3 xattrs: {error}")))?;
            metadata.insert("vfs-xattrs".to_string(), encoded);
        }
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .body(ByteStream::from(data.to_vec()))
            .set_metadata(Some(metadata))
            .send()
            .await
            .map_err(|err| s3_error(format!("put s3 object '{s3_key}': {err}")))?;
        Ok(())
    }

    async fn copy(&self, src: &str, dst: &str) -> VfsResult<()> {
        let src_key = self.key_for(src);
        let dst_key = self.key_for(dst);
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(format!("{}/{}", self.bucket, src_key))
            .key(&dst_key)
            .send()
            .await
            .map_err(|err| s3_error(format!("copy s3 object '{src_key}' to '{dst_key}': {err}")))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> VfsResult<()> {
        let s3_key = self.key_for(key);
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&s3_key)
            .send()
            .await
            .map_err(|err| s3_error(format!("delete s3 object '{s3_key}': {err}")))?;
        Ok(())
    }
}

fn kind_name(kind: InodeType) -> &'static str {
    match kind {
        InodeType::File => "file",
        InodeType::Directory => "directory",
        InodeType::Symlink => "symlink",
        InodeType::CharacterDevice => "character-device",
        InodeType::BlockDevice => "block-device",
        InodeType::Fifo => "fifo",
    }
}

fn timespec_from_smithy(time: &aws_sdk_s3::primitives::DateTime) -> Timespec {
    Timespec {
        sec: time.secs(),
        nsec: 0,
    }
}

fn format_timespec(time: Timespec) -> String {
    format!("{}:{}", time.sec, time.nsec)
}

fn parse_timespec(value: &str) -> Option<Timespec> {
    let (seconds, nanoseconds) = value.split_once(':')?;
    let sec = seconds.parse().ok()?;
    let nsec = nanoseconds.parse().ok()?;
    (nsec < 1_000_000_000).then_some(Timespec { sec, nsec })
}
