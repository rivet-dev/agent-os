use crate::engine::block::BlockStore;
use crate::engine::error::{VfsError, VfsResult};
use crate::engine::types::BlockKey;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct MemoryBlockStore {
    blocks: Arc<Mutex<BTreeMap<BlockKey, Vec<u8>>>>,
}

impl MemoryBlockStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.blocks.lock().expect("block mutex poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.blocks.lock().expect("block mutex poisoned").is_empty()
    }
}

#[async_trait]
impl BlockStore for MemoryBlockStore {
    async fn get(&self, key: &BlockKey) -> VfsResult<Vec<u8>> {
        self.blocks
            .lock()
            .expect("block mutex poisoned")
            .get(key)
            .cloned()
            .ok_or_else(|| VfsError::enoent(&key.0))
    }

    async fn get_range(&self, key: &BlockKey, off: u64, len: u64) -> VfsResult<Vec<u8>> {
        let data = self.get(key).await?;
        let start = usize::try_from(off)
            .map_err(|_| VfsError::einval(format!("range offset is too large: {off}")))?;
        let len = usize::try_from(len)
            .map_err(|_| VfsError::einval(format!("range length is too large: {len}")))?;
        if start >= data.len() {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(len).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn put(&self, key: &BlockKey, data: &[u8]) -> VfsResult<()> {
        self.blocks
            .lock()
            .expect("block mutex poisoned")
            .insert(key.clone(), data.to_vec());
        Ok(())
    }

    async fn exists(&self, key: &BlockKey) -> VfsResult<bool> {
        Ok(self
            .blocks
            .lock()
            .expect("block mutex poisoned")
            .contains_key(key))
    }

    async fn delete_many(&self, keys: &[BlockKey]) -> VfsResult<()> {
        let mut blocks = self.blocks.lock().expect("block mutex poisoned");
        for key in keys {
            blocks.remove(key);
        }
        Ok(())
    }

    async fn copy(&self, src: &BlockKey, dst: &BlockKey) -> VfsResult<()> {
        let data = self.get(src).await?;
        self.put(dst, &data).await
    }
}
