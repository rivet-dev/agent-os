use crate::engine::error::{VfsError, VfsResult};
use crate::engine::metadata::MetadataStore;
use crate::engine::types::{
    normalize_path, parent_and_name, BlockKey, ChunkEdit, ChunkRange, ChunkRef, CreateInodeAttrs,
    DentryStat, InodeMeta, InodePatch, InodeType, SnapshotId, Storage, Timespec,
    DEFAULT_CHUNK_SIZE, MAX_SYMLINK_DEPTH,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct InMemoryMetadataStore {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataDump {
    pub next_ino: u64,
    pub inodes: BTreeMap<u64, InodeMeta>,
    pub dentries: BTreeMap<(u64, String), u64>,
    pub chunks: BTreeMap<(u64, u64), ChunkRef>,
    pub block_refs: BTreeMap<BlockKey, u64>,
}

#[derive(Debug, Clone)]
struct State {
    next_ino: u64,
    next_snapshot_id: u64,
    inodes: BTreeMap<u64, InodeMeta>,
    dentries: BTreeMap<(u64, String), u64>,
    chunks: BTreeMap<(u64, u64), ChunkRef>,
    block_refs: BTreeMap<BlockKey, u64>,
    snapshots: BTreeMap<SnapshotId, Snapshot>,
}

#[derive(Debug, Clone)]
struct Snapshot {
    root_ino: u64,
    inodes: BTreeMap<u64, InodeMeta>,
    dentries: BTreeMap<(u64, String), u64>,
    chunks: BTreeMap<(u64, u64), ChunkRef>,
}

impl Default for InMemoryMetadataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryMetadataStore {
    pub const ROOT_INO: u64 = 1;

    pub fn new() -> Self {
        Self::new_with_root(0, 0, 0o755)
    }

    pub fn new_with_root(uid: u32, gid: u32, mode: u32) -> Self {
        let now = Timespec::now();
        let root = InodeMeta {
            ino: Self::ROOT_INO,
            kind: InodeType::Directory,
            mode,
            uid,
            gid,
            size: 0,
            nlink: 2,
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            storage: Storage::None,
            symlink_target: None,
            allocated_extents: Vec::new(),
            xattrs: BTreeMap::new(),
        };
        let mut inodes = BTreeMap::new();
        inodes.insert(Self::ROOT_INO, root);
        Self {
            state: Arc::new(Mutex::new(State {
                next_ino: 2,
                next_snapshot_id: 1,
                inodes,
                dentries: BTreeMap::new(),
                chunks: BTreeMap::new(),
                block_refs: BTreeMap::new(),
                snapshots: BTreeMap::new(),
            })),
        }
    }

    pub fn refcount(&self, key: &BlockKey) -> u64 {
        self.state
            .lock()
            .expect("metadata mutex poisoned")
            .block_refs
            .get(key)
            .copied()
            .unwrap_or(0)
    }

    pub fn dump(&self) -> MetadataDump {
        let state = self.state.lock().expect("metadata mutex poisoned");
        MetadataDump {
            next_ino: state.next_ino,
            inodes: state.inodes.clone(),
            dentries: state.dentries.clone(),
            chunks: state.chunks.clone(),
            block_refs: state.block_refs.clone(),
        }
    }

    pub fn inode_meta(&self, ino: u64) -> VfsResult<InodeMeta> {
        self.state
            .lock()
            .expect("metadata mutex poisoned")
            .inode(ino)
    }

    pub fn from_dump(dump: MetadataDump) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                next_ino: dump.next_ino,
                next_snapshot_id: 1,
                inodes: dump.inodes,
                dentries: dump.dentries,
                chunks: dump.chunks,
                block_refs: dump.block_refs,
                snapshots: BTreeMap::new(),
            })),
        }
    }
}

impl State {
    fn now_touch(meta: &mut InodeMeta) {
        let now = Timespec::now();
        meta.mtime = now;
        meta.ctime = now;
    }

    fn inode(&self, ino: u64) -> VfsResult<InodeMeta> {
        self.inodes
            .get(&ino)
            .cloned()
            .ok_or_else(|| VfsError::enoent(format!("inode {ino}")))
    }

    fn alloc_inode(&mut self, attrs: CreateInodeAttrs) -> InodeMeta {
        let ino = self.next_ino;
        self.next_ino += 1;
        let now = Timespec::now();
        let size = match &attrs.storage {
            Storage::Inline(data) => data.len() as u64,
            Storage::Chunked { .. } | Storage::None => attrs
                .symlink_target
                .as_ref()
                .map(|target| target.len() as u64)
                .unwrap_or(0),
        };
        let allocated_extents = match &attrs.storage {
            Storage::Inline(data) if !data.is_empty() => {
                vec![(0, (data.len() as u64).div_ceil(512))]
            }
            Storage::Inline(_) | Storage::Chunked { .. } | Storage::None => Vec::new(),
        };
        InodeMeta {
            ino,
            kind: attrs.kind,
            mode: attrs.mode,
            uid: attrs.uid,
            gid: attrs.gid,
            size,
            nlink: if attrs.kind == InodeType::Directory {
                2
            } else {
                1
            },
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            storage: attrs.storage,
            symlink_target: attrs.symlink_target,
            allocated_extents,
            xattrs: attrs.xattrs,
        }
    }

    fn name_child(&self, parent: u64, name: &str) -> VfsResult<u64> {
        self.dentries
            .get(&(parent, name.to_string()))
            .copied()
            .ok_or_else(|| VfsError::enoent(name))
    }

    fn resolve_path(&self, path: &str, follow_final: bool) -> VfsResult<InodeMeta> {
        self.resolve_path_depth(path, follow_final, 0)
    }

    fn resolve_path_depth(
        &self,
        path: &str,
        follow_final: bool,
        depth: usize,
    ) -> VfsResult<InodeMeta> {
        if depth > MAX_SYMLINK_DEPTH {
            return Err(VfsError::eloop(path));
        }
        let normalized = normalize_path(path)?;
        if normalized == "/" {
            return self.inode(InMemoryMetadataStore::ROOT_INO);
        }

        let parts: Vec<&str> = normalized.trim_start_matches('/').split('/').collect();
        let mut current = InMemoryMetadataStore::ROOT_INO;
        let mut prefix = String::new();
        for (idx, part) in parts.iter().enumerate() {
            let parent_meta = self.inode(current)?;
            if parent_meta.kind != InodeType::Directory {
                return Err(VfsError::enotdir(&prefix));
            }
            let child = self.name_child(current, part)?;
            let meta = self.inode(child)?;
            let final_component = idx == parts.len() - 1;
            if meta.kind == InodeType::Symlink && (!final_component || follow_final) {
                let target = meta.symlink_target.clone().unwrap_or_default();
                let rest = parts[idx + 1..].join("/");
                let base = if prefix.is_empty() { "/" } else { &prefix };
                let next_path = resolve_symlink_target(base, &target, &rest)?;
                return self.resolve_path_depth(&next_path, follow_final, depth + 1);
            }
            current = child;
            if prefix == "/" || prefix.is_empty() {
                prefix = format!("/{part}");
            } else {
                prefix.push('/');
                prefix.push_str(part);
            }
        }
        self.inode(current)
    }

    fn collect_subtree(&self, root: u64) -> VfsResult<BTreeSet<u64>> {
        let mut seen = BTreeSet::new();
        self.collect_subtree_into(root, &mut seen)?;
        Ok(seen)
    }

    fn collect_subtree_into(&self, ino: u64, seen: &mut BTreeSet<u64>) -> VfsResult<()> {
        if !seen.insert(ino) {
            return Ok(());
        }
        let meta = self.inode(ino)?;
        if meta.kind == InodeType::Directory {
            for ((parent, _), child) in self.dentries.range((ino, String::new())..) {
                if *parent != ino {
                    break;
                }
                self.collect_subtree_into(*child, seen)?;
            }
        }
        Ok(())
    }

    fn dec_block_ref(&mut self, key: &BlockKey, freed: &mut Vec<BlockKey>) {
        if let Some(refcount) = self.block_refs.get_mut(key) {
            *refcount = refcount.saturating_sub(1);
            if *refcount == 0 {
                self.block_refs.remove(key);
                freed.push(key.clone());
            }
        }
    }

    fn inc_block_ref(&mut self, key: &BlockKey) {
        *self.block_refs.entry(key.clone()).or_insert(0) += 1;
    }

    fn drop_inode_content(&mut self, ino: u64, freed: &mut Vec<BlockKey>) {
        let keys: Vec<BlockKey> = self
            .chunks
            .range((ino, 0)..)
            .take_while(|((chunk_ino, _), _)| *chunk_ino == ino)
            .map(|(_, chunk)| chunk.key.clone())
            .collect();
        self.chunks.retain(|(chunk_ino, _), _| *chunk_ino != ino);
        for key in keys {
            self.dec_block_ref(&key, freed);
        }
    }

    fn remove_child(&mut self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        let child = self.name_child(parent, name)?;
        let meta = self.inode(child)?;
        if meta.kind == InodeType::Directory
            && self
                .dentries
                .range((child, String::new())..)
                .next()
                .is_some_and(|((candidate_parent, _), _)| *candidate_parent == child)
        {
            return Err(VfsError::enotempty(name));
        }
        self.dentries.remove(&(parent, name.to_string()));
        let mut freed = Vec::new();
        if let Some(child_meta) = self.inodes.get_mut(&child) {
            child_meta.nlink = child_meta.nlink.saturating_sub(1);
            child_meta.ctime = Timespec::now();
            if child_meta.nlink > 0 {
                return Ok(freed);
            }
        }
        self.drop_inode_content(child, &mut freed);
        self.inodes.remove(&child);
        Ok(freed)
    }
}

fn resolve_symlink_target(base: &str, target: &str, rest: &str) -> VfsResult<String> {
    let mut path = if target.starts_with('/') {
        target.to_string()
    } else if base == "/" {
        format!("/{target}")
    } else {
        format!("{base}/{target}")
    };
    if !rest.is_empty() {
        if path != "/" {
            path.push('/');
        }
        path.push_str(rest);
    }
    normalize_path(&path)
}

#[async_trait]
impl MetadataStore for InMemoryMetadataStore {
    async fn resolve(&self, path: &str) -> VfsResult<InodeMeta> {
        self.state
            .lock()
            .expect("metadata mutex poisoned")
            .resolve_path(path, true)
    }

    async fn resolve_parent(&self, path: &str) -> VfsResult<(InodeMeta, String)> {
        let (parent, name) = parent_and_name(path)?;
        let parent_meta = self
            .state
            .lock()
            .expect("metadata mutex poisoned")
            .resolve_path(&parent, true)?;
        if parent_meta.kind != InodeType::Directory {
            return Err(VfsError::enotdir(parent));
        }
        Ok((parent_meta, name))
    }

    async fn lstat(&self, path: &str) -> VfsResult<InodeMeta> {
        self.state
            .lock()
            .expect("metadata mutex poisoned")
            .resolve_path(path, false)
    }

    async fn list_dir(&self, ino: u64) -> VfsResult<Vec<DentryStat>> {
        let state = self.state.lock().expect("metadata mutex poisoned");
        let meta = state.inode(ino)?;
        if meta.kind != InodeType::Directory {
            return Err(VfsError::enotdir(format!("inode {ino}")));
        }
        let mut entries = Vec::new();
        for ((parent, name), child) in state.dentries.range((ino, String::new())..) {
            if *parent != ino {
                break;
            }
            entries.push(DentryStat {
                name: name.clone(),
                meta: state.inode(*child)?,
            });
        }
        Ok(entries)
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        attrs: CreateInodeAttrs,
    ) -> VfsResult<InodeMeta> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let mut parent_meta = state.inode(parent)?;
        if parent_meta.kind != InodeType::Directory {
            return Err(VfsError::enotdir(format!("inode {parent}")));
        }
        if state.dentries.contains_key(&(parent, name.to_string())) {
            return Err(VfsError::eexist(name));
        }
        let meta = state.alloc_inode(attrs);
        let ino = meta.ino;
        state.inodes.insert(ino, meta.clone());
        state.dentries.insert((parent, name.to_string()), ino);
        State::now_touch(&mut parent_meta);
        state.inodes.insert(parent, parent_meta);
        Ok(meta)
    }

    async fn link(&self, parent: u64, name: &str, target: u64) -> VfsResult<()> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let mut parent_meta = state.inode(parent)?;
        if parent_meta.kind != InodeType::Directory {
            return Err(VfsError::enotdir(format!("inode {parent}")));
        }
        if state.dentries.contains_key(&(parent, name.to_string())) {
            return Err(VfsError::eexist(name));
        }
        let target_meta = state
            .inodes
            .get_mut(&target)
            .ok_or_else(|| VfsError::enoent(format!("inode {target}")))?;
        if target_meta.kind == InodeType::Directory {
            return Err(VfsError::eopnotsupp(
                "hard links to directories are not supported",
            ));
        }
        target_meta.nlink += 1;
        target_meta.ctime = Timespec::now();
        state.dentries.insert((parent, name.to_string()), target);
        State::now_touch(&mut parent_meta);
        state.inodes.insert(parent, parent_meta);
        Ok(())
    }

    async fn remove(&self, parent: u64, name: &str) -> VfsResult<Vec<BlockKey>> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let result = state.remove_child(parent, name)?;
        let parent_meta = state
            .inodes
            .get_mut(&parent)
            .ok_or_else(|| VfsError::enoent(format!("inode {parent}")))?;
        State::now_touch(parent_meta);
        Ok(result)
    }

    async fn rename(
        &self,
        src_parent: u64,
        src: &str,
        dst_parent: u64,
        dst: &str,
    ) -> VfsResult<Vec<BlockKey>> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let child = state.name_child(src_parent, src)?;
        let source_meta = state.inode(child)?;
        if source_meta.kind == InodeType::Directory {
            let descendant = state.collect_subtree(child)?.contains(&dst_parent);
            if descendant {
                return Err(VfsError::einval("cannot move a directory into itself"));
            }
        }
        let mut freed = Vec::new();
        if let Some(destination) = state.dentries.get(&(dst_parent, dst.to_string())).copied() {
            let destination_meta = state.inode(destination)?;
            match (
                source_meta.kind == InodeType::Directory,
                destination_meta.kind == InodeType::Directory,
            ) {
                (false, true) => return Err(VfsError::eisdir(dst)),
                (true, false) => return Err(VfsError::enotdir(dst)),
                _ => {}
            }
            freed = state.remove_child(dst_parent, dst)?;
        }
        state.dentries.remove(&(src_parent, src.to_string()));
        state.dentries.insert((dst_parent, dst.to_string()), child);
        let now = Timespec::now();
        state
            .inodes
            .get_mut(&child)
            .ok_or_else(|| VfsError::enoent(format!("inode {child}")))?
            .ctime = now;
        for parent in [src_parent, dst_parent] {
            let parent_meta = state
                .inodes
                .get_mut(&parent)
                .ok_or_else(|| VfsError::enoent(format!("inode {parent}")))?;
            parent_meta.mtime = now;
            parent_meta.ctime = now;
        }
        Ok(freed)
    }

    async fn set_attr(&self, ino: u64, patch: InodePatch) -> VfsResult<Vec<BlockKey>> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let access_time_only = patch.atime.is_some()
            && patch.mode.is_none()
            && patch.uid.is_none()
            && patch.gid.is_none()
            && patch.mtime.is_none()
            && patch.storage.is_none()
            && patch.size.is_none()
            && patch.allocated_extents.is_none()
            && patch.xattrs.is_none();
        let content_changed =
            patch.storage.is_some() || patch.size.is_some() || patch.allocated_extents.is_some();
        let explicit_mtime = patch.mtime.is_some();
        let mut freed = Vec::new();
        if let Some(storage) = &patch.storage {
            if matches!(storage, Storage::Inline(_) | Storage::None) {
                state.drop_inode_content(ino, &mut freed);
            }
        }
        let meta = state
            .inodes
            .get_mut(&ino)
            .ok_or_else(|| VfsError::enoent(format!("inode {ino}")))?;
        if let Some(mode) = patch.mode {
            meta.mode = mode;
        }
        if let Some(uid) = patch.uid {
            meta.uid = uid;
        }
        if let Some(gid) = patch.gid {
            meta.gid = gid;
        }
        if let Some(atime) = patch.atime {
            meta.atime = atime;
        }
        if let Some(mtime) = patch.mtime {
            meta.mtime = mtime;
        }
        if let Some(storage) = patch.storage {
            meta.size = match &storage {
                Storage::Inline(data) => data.len() as u64,
                Storage::Chunked { .. } => patch.size.unwrap_or(meta.size),
                Storage::None => meta
                    .symlink_target
                    .as_ref()
                    .map_or(0, |target| target.len() as u64),
            };
            if patch.allocated_extents.is_none() {
                match &storage {
                    Storage::Inline(data) => {
                        meta.allocated_extents = if data.is_empty() {
                            Vec::new()
                        } else {
                            vec![(0, (data.len() as u64).div_ceil(512))]
                        };
                    }
                    Storage::None => meta.allocated_extents.clear(),
                    Storage::Chunked { .. } => {}
                }
            }
            meta.storage = storage;
        }
        if let Some(allocated_extents) = patch.allocated_extents {
            meta.allocated_extents = allocated_extents;
        }
        if let Some(xattrs) = patch.xattrs {
            meta.xattrs = xattrs;
        }
        if let Some(size) = patch.size {
            meta.size = size;
        }
        let now = Timespec::now();
        if content_changed && !explicit_mtime {
            meta.mtime = now;
        }
        if !access_time_only {
            meta.ctime = now;
        }
        Ok(freed)
    }

    async fn commit_write(
        &self,
        ino: u64,
        edits: Vec<ChunkEdit>,
        new_size: u64,
        allocated_extents: Vec<(u64, u64)>,
    ) -> VfsResult<Vec<BlockKey>> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let meta = state.inode(ino)?;
        let chunk_size = match meta.storage {
            Storage::Chunked { chunk_size } => u64::from(chunk_size),
            Storage::Inline(_) | Storage::None => DEFAULT_CHUNK_SIZE as u64,
        };
        let mut freed = Vec::new();
        let keep_chunks = if new_size == 0 {
            0
        } else {
            new_size.div_ceil(chunk_size)
        };
        let truncated: Vec<(u64, BlockKey)> = state
            .chunks
            .range((ino, 0)..)
            .take_while(|((chunk_ino, _), _)| *chunk_ino == ino)
            .filter(|((_, index), _)| *index >= keep_chunks)
            .map(|((_, index), chunk)| (*index, chunk.key.clone()))
            .collect();
        for (index, key) in truncated {
            state.chunks.remove(&(ino, index));
            state.dec_block_ref(&key, &mut freed);
        }
        for edit in edits {
            if edit.index >= keep_chunks {
                continue;
            }
            let key = edit.key.clone();
            if let Some(previous) = state.chunks.insert(
                (ino, edit.index),
                ChunkRef {
                    index: edit.index,
                    key: edit.key,
                    len: edit.len,
                },
            ) {
                if previous.key != key {
                    state.dec_block_ref(&previous.key, &mut freed);
                    state.inc_block_ref(&key);
                }
            } else {
                state.inc_block_ref(&key);
            }
        }
        // A range shift can release a content-addressed block at one index and
        // reintroduce the same key at another index in this commit. Only return
        // keys that are still unreferenced after every edit has been applied.
        freed.retain(|key| !state.block_refs.contains_key(key));
        let meta = state
            .inodes
            .get_mut(&ino)
            .ok_or_else(|| VfsError::enoent(format!("inode {ino}")))?;
        meta.size = new_size;
        meta.allocated_extents = allocated_extents;
        meta.ctime = Timespec::now();
        meta.mtime = meta.ctime;
        Ok(freed)
    }

    async fn get_chunks(&self, ino: u64, range: ChunkRange) -> VfsResult<Vec<ChunkRef>> {
        let state = self.state.lock().expect("metadata mutex poisoned");
        state.inode(ino)?;
        let mut chunks = Vec::new();
        for ((chunk_ino, index), chunk) in state.chunks.range((ino, range.start)..) {
            if *chunk_ino != ino {
                break;
            }
            if let Some(end) = range.end {
                if *index >= end {
                    break;
                }
            }
            chunks.push(chunk.clone());
        }
        Ok(chunks)
    }

    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let reachable = state.collect_subtree(root)?;
        let mut inodes = BTreeMap::new();
        let mut dentries = BTreeMap::new();
        let mut chunks = BTreeMap::new();
        let mut keys_to_inc = Vec::new();
        for ino in reachable {
            inodes.insert(ino, state.inode(ino)?);
            for ((parent, name), child) in state.dentries.range((ino, String::new())..) {
                if *parent != ino {
                    break;
                }
                dentries.insert((*parent, name.clone()), *child);
            }
            for ((chunk_ino, index), chunk) in &state.chunks {
                if *chunk_ino == ino {
                    chunks.insert((*chunk_ino, *index), chunk.clone());
                    keys_to_inc.push(chunk.key.clone());
                }
            }
        }
        for key in keys_to_inc {
            state.inc_block_ref(&key);
        }
        let id = SnapshotId(state.next_snapshot_id);
        state.next_snapshot_id += 1;
        state.snapshots.insert(
            id,
            Snapshot {
                root_ino: root,
                inodes,
                dentries,
                chunks,
            },
        );
        Ok(id)
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        let mut state = self.state.lock().expect("metadata mutex poisoned");
        let snapshot = state
            .snapshots
            .get(&snap)
            .cloned()
            .ok_or_else(|| VfsError::enoent(format!("snapshot {}", snap.0)))?;
        let mut map = BTreeMap::new();
        for old_ino in snapshot.inodes.keys() {
            let new_ino = state.next_ino;
            state.next_ino += 1;
            map.insert(*old_ino, new_ino);
        }
        for (old_ino, mut meta) in snapshot.inodes {
            meta.ino = map[&old_ino];
            state.inodes.insert(meta.ino, meta);
        }
        for ((old_parent, name), old_child) in snapshot.dentries {
            if let (Some(parent), Some(child)) = (map.get(&old_parent), map.get(&old_child)) {
                state.dentries.insert((*parent, name), *child);
            }
        }
        for ((old_ino, index), mut chunk) in snapshot.chunks {
            if let Some(new_ino) = map.get(&old_ino) {
                let key = chunk.key.clone();
                chunk.index = index;
                state.chunks.insert((*new_ino, index), chunk);
                state.inc_block_ref(&key);
            }
        }
        Ok(map[&snapshot.root_ino])
    }

    async fn gc(&self) -> VfsResult<Vec<BlockKey>> {
        Ok(Vec::new())
    }
}
