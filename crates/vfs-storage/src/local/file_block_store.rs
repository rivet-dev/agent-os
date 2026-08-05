use agentos_vfs_core::engine::block::BlockStore;
use agentos_vfs_core::engine::error::{VfsError, VfsResult};
use agentos_vfs_core::engine::types::BlockKey;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct FileBlockStore {
    root: PathBuf,
    cache: Arc<Mutex<BlockCache>>,
}

const DEFAULT_BLOCK_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
struct BlockCache {
    entries: HashMap<BlockKey, Vec<u8>>,
    insertion_order: VecDeque<BlockKey>,
    bytes: usize,
    max_bytes: usize,
}

impl BlockCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            bytes: 0,
            max_bytes,
        }
    }

    fn get(&self, key: &BlockKey) -> Option<Vec<u8>> {
        self.entries.get(key).cloned()
    }

    fn contains(&self, key: &BlockKey) -> bool {
        self.entries.contains_key(key)
    }

    fn insert(&mut self, key: BlockKey, data: &[u8]) {
        if data.len() > self.max_bytes {
            return;
        }
        if let Some(previous) = self.entries.insert(key.clone(), data.to_vec()) {
            self.bytes = self.bytes.saturating_sub(previous.len());
        } else {
            self.insertion_order.push_back(key);
        }
        self.bytes = self.bytes.saturating_add(data.len());
        while self.bytes > self.max_bytes {
            let Some(oldest) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(removed) = self.entries.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(removed.len());
            }
        }
    }

    fn remove(&mut self, key: &BlockKey) {
        if let Some(removed) = self.entries.remove(key) {
            self.bytes = self.bytes.saturating_sub(removed.len());
        }
    }
}

impl FileBlockStore {
    pub fn new(root: impl Into<PathBuf>) -> VfsResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|err| VfsError::eio(format!("create block root {}: {err}", root.display())))?;
        Ok(Self {
            root,
            cache: Arc::new(Mutex::new(BlockCache::new(DEFAULT_BLOCK_CACHE_BYTES))),
        })
    }

    fn path_for(&self, key: &BlockKey) -> PathBuf {
        block_path(&self.root, key)
    }

    fn ensure_safe_key(key: &BlockKey) -> VfsResult<()> {
        if key.0.contains('/') || key.0.contains('\\') || key.0 == "." || key.0 == ".." {
            return Err(VfsError::einval(format!("unsafe block key: {}", key.0)));
        }
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl BlockStore for FileBlockStore {
    async fn get(&self, key: &BlockKey) -> VfsResult<Vec<u8>> {
        Self::ensure_safe_key(key)?;
        if let Some(data) = self
            .cache
            .lock()
            .expect("block cache mutex poisoned")
            .get(key)
        {
            return Ok(data);
        }
        let path = self.path_for(key);
        let data = fs::read(&path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                VfsError::enoent(&key.0)
            } else {
                VfsError::eio(format!("read block {}: {err}", path.display()))
            }
        })?;
        self.cache
            .lock()
            .expect("block cache mutex poisoned")
            .insert(key.clone(), &data);
        Ok(data)
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
        Self::ensure_safe_key(key)?;
        let path = self.path_for(key);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| VfsError::eio(format!("create block dir: {err}")))?;
        }
        fs::write(&path, data)
            .map_err(|err| VfsError::eio(format!("write block {}: {err}", path.display())))?;
        self.cache
            .lock()
            .expect("block cache mutex poisoned")
            .insert(key.clone(), data);
        Ok(())
    }

    async fn exists(&self, key: &BlockKey) -> VfsResult<bool> {
        Self::ensure_safe_key(key)?;
        if self
            .cache
            .lock()
            .expect("block cache mutex poisoned")
            .contains(key)
        {
            return Ok(true);
        }
        Ok(self.path_for(key).exists())
    }

    async fn delete_many(&self, keys: &[BlockKey]) -> VfsResult<()> {
        let mut errors = Vec::new();
        for key in keys {
            if let Err(error) = Self::ensure_safe_key(key) {
                errors.push(error.to_string());
                continue;
            }
            self.cache
                .lock()
                .expect("block cache mutex poisoned")
                .remove(key);
            match fs::remove_file(self.path_for(key)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => errors.push(format!("delete block {}: {err}", key.0)),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(VfsError::eio(format!(
                "delete {} local blocks failed: {}",
                errors.len(),
                errors.join("; ")
            )))
        }
    }

    async fn copy(&self, src: &BlockKey, dst: &BlockKey) -> VfsResult<()> {
        let data = self.get(src).await?;
        self.put(dst, &data).await
    }

    async fn sync(&self) -> VfsResult<()> {
        for prefix in fs::read_dir(&self.root).map_err(|err| {
            VfsError::eio(format!("read block root {}: {err}", self.root.display()))
        })? {
            let prefix = prefix.map_err(|err| {
                VfsError::eio(format!(
                    "read block root entry {}: {err}",
                    self.root.display()
                ))
            })?;
            let file_type = prefix.file_type().map_err(|err| {
                VfsError::eio(format!(
                    "stat block entry {}: {err}",
                    prefix.path().display()
                ))
            })?;
            if !file_type.is_dir() {
                continue;
            }
            for block in fs::read_dir(prefix.path()).map_err(|err| {
                VfsError::eio(format!(
                    "read block directory {}: {err}",
                    prefix.path().display()
                ))
            })? {
                let block = block
                    .map_err(|err| VfsError::eio(format!("read block directory entry: {err}")))?;
                if block
                    .file_type()
                    .map_err(|err| {
                        VfsError::eio(format!("stat block {}: {err}", block.path().display()))
                    })?
                    .is_file()
                {
                    fs::File::open(block.path())
                        .and_then(|file| file.sync_all())
                        .map_err(|err| {
                            VfsError::eio(format!("sync block {}: {err}", block.path().display()))
                        })?;
                }
            }
            fs::File::open(prefix.path())
                .and_then(|directory| directory.sync_all())
                .map_err(|err| {
                    VfsError::eio(format!(
                        "sync block directory {}: {err}",
                        prefix.path().display()
                    ))
                })?;
        }
        fs::File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|err| VfsError::eio(format!("sync block root {}: {err}", self.root.display())))
    }
}

fn block_path(root: &Path, key: &BlockKey) -> PathBuf {
    let (prefix, suffix) = key.0.split_at(key.0.len().min(2));
    root.join(prefix).join(suffix)
}
