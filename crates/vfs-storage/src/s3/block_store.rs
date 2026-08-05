use agentos_vfs_core::engine::{BlockKey, BlockStore, VfsError, VfsResult};
use async_trait::async_trait;
use aws_sdk_s3::primitives::{ByteStream, ByteStreamError};
use aws_sdk_s3::types::{Delete, ObjectIdentifier};
use aws_sdk_s3::Client;

#[derive(Debug, Clone, Default)]
pub struct S3BlockStoreOptions {
    pub prefix: String,
}

#[derive(Debug, Clone)]
pub struct S3BlockStore {
    client: Client,
    bucket: String,
    options: S3BlockStoreOptions,
}

impl S3BlockStore {
    pub fn new(client: Client, bucket: impl Into<String>) -> Self {
        Self::with_options(client, bucket, S3BlockStoreOptions::default())
    }

    pub fn with_options(
        client: Client,
        bucket: impl Into<String>,
        options: S3BlockStoreOptions,
    ) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            options,
        }
    }

    fn key_for(&self, key: &BlockKey) -> String {
        format!("{}{}", self.options.prefix, key.0)
    }
}

#[async_trait]
impl BlockStore for S3BlockStore {
    async fn get(&self, key: &BlockKey) -> VfsResult<Vec<u8>> {
        let object_key = self.key_for(key);
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .send()
            .await
            .map_err(|err| s3_error(format!("get s3 block '{object_key}': {err}")))?;
        collect_body(response.body, &object_key).await
    }

    async fn get_range(&self, key: &BlockKey, off: u64, len: u64) -> VfsResult<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let object_key = self.key_for(key);
        let end = off
            .checked_add(len)
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| VfsError::einval("invalid S3 byte range"))?;
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .range(format!("bytes={off}-{end}"))
            .send()
            .await
            .map_err(|err| s3_error(format!("get s3 block range '{object_key}': {err}")))?;
        collect_body(response.body, &object_key).await
    }

    async fn put(&self, key: &BlockKey, data: &[u8]) -> VfsResult<()> {
        let object_key = self.key_for(key);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .body(ByteStream::from(data.to_vec()))
            .send()
            .await
            .map_err(|err| s3_error(format!("put s3 block '{object_key}': {err}")))?;
        Ok(())
    }

    async fn exists(&self, key: &BlockKey) -> VfsResult<bool> {
        let object_key = self.key_for(key);
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&object_key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(err)
                if err.as_service_error().is_some_and(|err| {
                    err.is_not_found()
                        || matches!(err.meta().code(), Some("NotFound" | "NoSuchKey"))
                }) =>
            {
                Ok(false)
            }
            Err(err) => Err(s3_error(format!("head s3 block '{object_key}': {err}"))),
        }
    }

    async fn delete_many(&self, keys: &[BlockKey]) -> VfsResult<()> {
        let mut errors = Vec::new();
        for chunk in keys.chunks(1000) {
            let mut object_keys = Vec::with_capacity(chunk.len());
            let mut objects = Vec::with_capacity(chunk.len());
            for key in chunk {
                let object_key = self.key_for(key);
                let object = ObjectIdentifier::builder()
                    .key(&object_key)
                    .build()
                    .map_err(|err| {
                        s3_error(format!("build s3 delete key '{object_key}': {err}"))
                    })?;
                object_keys.push(object_key);
                objects.push(object);
            }
            let delete = Delete::builder()
                .set_objects(Some(objects))
                .quiet(true)
                .build()
                .map_err(|err| s3_error(format!("build s3 delete batch: {err}")))?;
            match self
                .client
                .delete_objects()
                .bucket(&self.bucket)
                .delete(delete)
                .send()
                .await
            {
                Ok(response) => {
                    for error in response.errors() {
                        let key = error.key().unwrap_or("<unknown>");
                        let code = error.code().unwrap_or("unknown");
                        let message = error.message().unwrap_or("unknown error");
                        errors.push(format!("delete s3 block '{key}': {code}: {message}"));
                    }
                }
                Err(err) => errors.push(format!(
                    "delete s3 block batch [{}]: {err}",
                    object_keys.join(", ")
                )),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(s3_error(format!(
                "delete {} s3 blocks failed: {}",
                errors.len(),
                errors.join("; ")
            )))
        }
    }

    async fn copy(&self, src: &BlockKey, dst: &BlockKey) -> VfsResult<()> {
        let src_key = self.key_for(src);
        let dst_key = self.key_for(dst);
        self.client
            .copy_object()
            .bucket(&self.bucket)
            .copy_source(format!("{}/{}", self.bucket, src_key))
            .key(&dst_key)
            .send()
            .await
            .map_err(|err| s3_error(format!("copy s3 block '{src_key}' to '{dst_key}': {err}")))?;
        Ok(())
    }
}

pub(crate) async fn collect_body(body: ByteStream, key: &str) -> VfsResult<Vec<u8>> {
    body.collect()
        .await
        .map_err(|err| body_error(key, err))
        .map(|bytes| bytes.into_bytes().to_vec())
}

pub(crate) fn s3_error(message: impl Into<String>) -> VfsError {
    VfsError::eio(message)
}

fn body_error(key: &str, err: ByteStreamError) -> VfsError {
    VfsError::eio(format!("read s3 object '{key}': {err}"))
}
