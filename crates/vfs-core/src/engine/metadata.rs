use crate::engine::error::VfsResult;
use crate::engine::types::{
    BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs, DentryStat, InodeMeta, InodePatch,
    SnapshotId,
};
use async_trait::async_trait;

#[async_trait]
pub trait MetadataStore: Send + Sync {
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta>;
    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)>;
    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta>;
    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>>;

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta>;
    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()>;
    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>>;
    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>>;
    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>>;
    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>>;

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>>;

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId>;
    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64>;
    async fn gc(&self) -> VfsResult<Vec<BlockKey>>;

    async fn flush(&self) -> VfsResult<()> {
        Ok(())
    }
}
