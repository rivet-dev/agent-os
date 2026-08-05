use crate::engine::error::VfsResult;
use crate::engine::metadata::MetadataStore;
use crate::engine::types::{
    BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs, DentryStat, InodeMeta, InodePatch,
    SnapshotId,
};
use async_trait::async_trait;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

pub struct CachedMetadataStore<M> {
    inner: M,
    cache: Mutex<CacheState>,
}

#[derive(Debug)]
struct CacheState {
    capacity: usize,
    generation: u64,
    resolve: BTreeMap<String, Option<InodeMeta>>,
    lstat: BTreeMap<String, Option<InodeMeta>>,
    list_dir: BTreeMap<u64, Vec<DentryStat>>,
    order: VecDeque<CacheKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CacheKey {
    Resolve(String),
    Lstat(String),
    ListDir(u64),
}

impl<M> CachedMetadataStore<M> {
    pub fn new(inner: M, capacity: usize) -> Self {
        Self {
            inner,
            cache: Mutex::new(CacheState {
                capacity,
                generation: 0,
                resolve: BTreeMap::new(),
                lstat: BTreeMap::new(),
                list_dir: BTreeMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    pub fn into_inner(self) -> M {
        self.inner
    }

    fn clear_after_mutation(&self) {
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        cache.generation = cache.generation.wrapping_add(1);
        cache.resolve.clear();
        cache.lstat.clear();
        cache.list_dir.clear();
        cache.order.clear();
    }
}

impl CacheState {
    fn remember(&mut self, key: CacheKey) {
        if self.capacity == 0 {
            self.resolve.clear();
            self.lstat.clear();
            self.list_dir.clear();
            self.order.clear();
            return;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                match evicted {
                    CacheKey::Resolve(path) => {
                        self.resolve.remove(&path);
                    }
                    CacheKey::Lstat(path) => {
                        self.lstat.remove(&path);
                    }
                    CacheKey::ListDir(ino) => {
                        self.list_dir.remove(&ino);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl<M: MetadataStore> MetadataStore for CachedMetadataStore<M> {
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta> {
        let generation = {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if let Some(cached) = cache.resolve.get(path).cloned() {
                return cached.ok_or_else(|| crate::engine::error::VfsError::enoent(path));
            }
            cache.generation
        };
        let result = self.inner.resolve(path).await;
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        if cache.generation == generation {
            let cached = match &result {
                Ok(meta) => Some(Some(meta.clone())),
                Err(error) if error.code() == "ENOENT" => Some(None),
                Err(_) => None,
            };
            if let Some(cached) = cached {
                cache.resolve.insert(path.to_string(), cached);
                cache.remember(CacheKey::Resolve(path.to_string()));
            }
        }
        result
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)> {
        self.inner.resolve_parent(path).await
    }

    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta> {
        let generation = {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if let Some(cached) = cache.lstat.get(path).cloned() {
                return cached.ok_or_else(|| crate::engine::error::VfsError::enoent(path));
            }
            cache.generation
        };
        let result = self.inner.lstat(path).await;
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        if cache.generation == generation {
            let cached = match &result {
                Ok(meta) => Some(Some(meta.clone())),
                Err(error) if error.code() == "ENOENT" => Some(None),
                Err(_) => None,
            };
            if let Some(cached) = cached {
                cache.lstat.insert(path.to_string(), cached);
                cache.remember(CacheKey::Lstat(path.to_string()));
            }
        }
        result
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>> {
        let generation = {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if let Some(cached) = cache.list_dir.get(&ino).cloned() {
                return Ok(cached);
            }
            cache.generation
        };
        let entries = self.inner.list_dir(ino).await?;
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        if cache.generation == generation {
            cache.list_dir.insert(ino, entries.clone());
            cache.remember(CacheKey::ListDir(ino));
        }
        Ok(entries)
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta> {
        let result = self.inner.create(parent, name, attrs).await;
        self.clear_after_mutation();
        result
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        let result = self.inner.link(parent, name, target).await;
        self.clear_after_mutation();
        result
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        let result = self.inner.remove(parent, name).await;
        self.clear_after_mutation();
        result
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        let result = self.inner.rename(src_parent, src, dst_parent, dst).await;
        self.clear_after_mutation();
        result
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        let result = self.inner.set_attr(ino, patch).await;
        self.clear_after_mutation();
        result
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        let result = self
            .inner
            .commit_write(ino, edits, new_size, allocated_extents)
            .await;
        self.clear_after_mutation();
        result
    }

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>> {
        self.inner.get_chunks(ino, range).await
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        self.inner.snapshot(root).await
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        self.inner.fork(snap).await
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        self.inner.gc().await
    }

    async fn flush(&self) -> VfsResult<()> {
        self.inner.flush().await
    }
}
