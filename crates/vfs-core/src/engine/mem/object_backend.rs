use crate::engine::block::ObjectBackend;
use crate::engine::error::{VfsError, VfsResult};
use crate::engine::types::{ObjectEntry, ObjectMeta};
use async_trait::async_trait;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
struct StoredObject {
    data: Vec<u8>,
    meta: ObjectMeta,
}

#[derive(Debug, Clone, Default)]
pub struct MemoryObjectBackend {
    objects: Arc<Mutex<BTreeMap<String, StoredObject>>>,
}

impl MemoryObjectBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ObjectBackend for MemoryObjectBackend {
    async fn list(&self, prefix: &str) -> VfsResult<Vec<ObjectEntry>> {
        let objects = self.objects.lock().expect("object mutex poisoned");
        let mut entries = Vec::new();
        let mut prefixes = BTreeSet::new();
        for (key, object) in objects.range(prefix.to_string()..) {
            if !key.starts_with(prefix) {
                break;
            }
            let rest = &key[prefix.len()..];
            if rest.is_empty() {
                entries.push(ObjectEntry {
                    name: key.clone(),
                    size: object.meta.size,
                    mtime: object.meta.mtime,
                    is_prefix: false,
                });
                continue;
            }
            if let Some((head, _)) = rest.split_once('/') {
                prefixes.insert(format!("{prefix}{head}/"));
            } else {
                entries.push(ObjectEntry {
                    name: key.clone(),
                    size: object.meta.size,
                    mtime: object.meta.mtime,
                    is_prefix: false,
                });
            }
        }
        entries.extend(prefixes.into_iter().map(|name| ObjectEntry {
            name,
            size: 0,
            mtime: crate::engine::types::Timespec::now(),
            is_prefix: true,
        }));
        Ok(entries)
    }

    async fn head(&self, key: &str) -> VfsResult<Option<ObjectMeta>> {
        Ok(self
            .objects
            .lock()
            .expect("object mutex poisoned")
            .get(key)
            .map(|object| object.meta.clone()))
    }

    async fn get_range(&self, key: &str, off: u64, len: u64) -> VfsResult<Vec<u8>> {
        let objects = self.objects.lock().expect("object mutex poisoned");
        let object = objects.get(key).ok_or_else(|| VfsError::enoent(key))?;
        let start = usize::try_from(off)
            .map_err(|_| VfsError::einval(format!("range offset is too large: {off}")))?;
        let len = usize::try_from(len)
            .map_err(|_| VfsError::einval(format!("range length is too large: {len}")))?;
        if start >= object.data.len() {
            return Ok(Vec::new());
        }
        let end = start.saturating_add(len).min(object.data.len());
        Ok(object.data[start..end].to_vec())
    }

    async fn put(&self, key: &str, data: &[u8], mut meta: ObjectMeta) -> VfsResult<()> {
        meta.size = data.len() as u64;
        self.objects.lock().expect("object mutex poisoned").insert(
            key.to_string(),
            StoredObject {
                data: data.to_vec(),
                meta,
            },
        );
        Ok(())
    }

    async fn copy(&self, src: &str, dst: &str) -> VfsResult<()> {
        let mut objects = self.objects.lock().expect("object mutex poisoned");
        let object = objects
            .get(src)
            .cloned()
            .ok_or_else(|| VfsError::enoent(src))?;
        objects.insert(dst.to_string(), object);
        Ok(())
    }

    async fn delete(&self, key: &str) -> VfsResult<()> {
        self.objects
            .lock()
            .expect("object mutex poisoned")
            .remove(key);
        Ok(())
    }
}
