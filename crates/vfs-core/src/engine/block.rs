use crate::engine::error::VfsResult;
use crate::engine::types::{BlockKey, ObjectEntry, ObjectMeta};
use async_trait::async_trait;

#[async_trait]
pub trait BlockStore: Send + Sync {
    async fn get(&self, key: &BlockKey) -> VfsResult<Vec<u8>>;
    async fn get_range(&self, key: &BlockKey, off: u64, len: u64) -> VfsResult<Vec<u8>>;
    async fn put(&self, key: &BlockKey, data: &[u8]) -> VfsResult<()>;
    async fn exists(&self, key: &BlockKey) -> VfsResult<bool>;
    async fn delete_many(&self, keys: &[BlockKey]) -> VfsResult<()>;
    async fn copy(&self, src: &BlockKey, dst: &BlockKey) -> VfsResult<()>;

    async fn sync(&self) -> VfsResult<()> {
        Ok(())
    }
}

#[async_trait]
pub trait ObjectBackend: Send + Sync {
    async fn list(&self, prefix: &str) -> VfsResult<Vec<ObjectEntry>>;
    async fn head(&self, key: &str) -> VfsResult<Option<ObjectMeta>>;
    async fn get_range(&self, key: &str, off: u64, len: u64) -> VfsResult<Vec<u8>>;
    async fn put(&self, key: &str, data: &[u8], meta: ObjectMeta) -> VfsResult<()>;
    async fn copy(&self, src: &str, dst: &str) -> VfsResult<()>;
    async fn delete(&self, key: &str) -> VfsResult<()>;
}
