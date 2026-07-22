use crate::engine::block::BlockStore;
use crate::engine::error::{VfsError, VfsResult};
use crate::engine::metadata::MetadataStore;
use crate::engine::types::{
    decode_unwritten_extents, encode_unwritten_extents, normalize_path, set_xattr_value,
    unwritten_after_allocate, unwritten_after_collapse, unwritten_after_insert,
    unwritten_after_truncate, unwritten_after_write, unwritten_after_zero, unwritten_byte_ranges,
    unwritten_sector_ranges, validate_xattr_name, BlockKey, ChunkEdit, ChunkRange,
    CreateInodeAttrs, Dentry, FileExtent, InodeMeta, InodePatch, InodeType, SnapshotId, Storage,
    Timespec, VirtualStat, DEFAULT_CHUNK_SIZE, DEFAULT_INLINE_THRESHOLD, INTERNAL_XATTR_PREFIX,
};
use crate::engine::vfs::{Snapshottable, VirtualFileSystem};
use async_trait::async_trait;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ChunkedFsOptions {
    pub inline_threshold: usize,
    pub chunk_size: u32,
    pub uid: u32,
    pub gid: u32,
    pub file_mode: u32,
    pub dir_mode: u32,
}

impl Default for ChunkedFsOptions {
    fn default() -> Self {
        Self {
            inline_threshold: DEFAULT_INLINE_THRESHOLD,
            chunk_size: DEFAULT_CHUNK_SIZE,
            uid: 0,
            gid: 0,
            file_mode: 0o644,
            dir_mode: 0o755,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChunkedFs<M, B> {
    metadata: M,
    blocks: B,
    options: ChunkedFsOptions,
    adaptive_chunk_size: bool,
}

const MAX_ADAPTIVE_CHUNK_SIZE: u32 = 1024 * 1024;

impl<M, B> ChunkedFs<M, B> {
    pub fn new(metadata: M, blocks: B) -> Self {
        Self::with_options(metadata, blocks, ChunkedFsOptions::default())
    }

    pub fn with_options(metadata: M, blocks: B, options: ChunkedFsOptions) -> Self {
        Self {
            metadata,
            blocks,
            options,
            adaptive_chunk_size: false,
        }
    }

    pub fn with_adaptive_chunk_size(metadata: M, blocks: B, options: ChunkedFsOptions) -> Self {
        Self {
            metadata,
            blocks,
            options,
            adaptive_chunk_size: true,
        }
    }

    pub fn metadata(&self) -> &M {
        &self.metadata
    }

    pub fn blocks(&self) -> &B {
        &self.blocks
    }
}

impl<M: MetadataStore, B: BlockStore> ChunkedFs<M, B> {
    fn initial_chunk_size(&self, write_len: usize) -> u32 {
        if !self.adaptive_chunk_size {
            return self.options.chunk_size;
        }
        let write_len = u32::try_from(write_len).unwrap_or(u32::MAX);
        write_len
            .max(self.options.chunk_size)
            .min(MAX_ADAPTIVE_CHUNK_SIZE.max(self.options.chunk_size))
    }

    async fn write_existing_or_create(&self, path: &str, content: &[u8]) -> VfsResult<()> {
        let (parent, name) = self.metadata.resolve_parent(path).await?;
        let existing = self.metadata.lstat(path).await.ok();
        let (ino, mut xattrs) = match existing {
            Some(meta) => {
                if meta.kind == InodeType::Directory {
                    return Err(VfsError::eisdir(path));
                }
                (meta.ino, meta.xattrs)
            }
            None => {
                let chunk_size = self.initial_chunk_size(content.len());
                let storage = if content.len() <= self.options.inline_threshold {
                    Storage::Inline(content.to_vec())
                } else {
                    Storage::Chunked { chunk_size }
                };
                let meta = self
                    .metadata
                    .create(
                        parent.ino,
                        &name,
                        CreateInodeAttrs::file(
                            self.options.file_mode,
                            self.options.uid,
                            self.options.gid,
                            storage,
                        ),
                    )
                    .await?;
                if content.len() <= self.options.inline_threshold {
                    return Ok(());
                }
                (meta.ino, meta.xattrs)
            }
        };
        encode_unwritten_extents(&mut xattrs, &[]);

        if content.len() <= self.options.inline_threshold {
            let freed = self
                .metadata
                .set_attr(
                    ino,
                    InodePatch {
                        storage: Some(Storage::Inline(content.to_vec())),
                        size: Some(content.len() as u64),
                        xattrs: Some(xattrs),
                        ..InodePatch::default()
                    },
                )
                .await?;
            self.blocks.delete_many(&freed).await?;
            return Ok(());
        }

        let chunk_size = self.initial_chunk_size(content.len());
        let mut edits = Vec::new();
        for (index, chunk) in content.chunks(chunk_size as usize).enumerate() {
            let key = BlockKey::from_content(chunk);
            if !self.blocks.exists(&key).await? {
                self.blocks.put(&key, chunk).await?;
            }
            edits.push(ChunkEdit {
                index: index as u64,
                key,
                len: chunk.len() as u32,
            });
        }
        self.metadata
            .set_attr(
                ino,
                InodePatch {
                    storage: Some(Storage::Chunked { chunk_size }),
                    size: Some(content.len() as u64),
                    xattrs: Some(xattrs),
                    ..InodePatch::default()
                },
            )
            .await?;
        let freed = self
            .metadata
            .commit_write(
                ino,
                edits,
                content.len() as u64,
                dense_allocation(content.len() as u64),
            )
            .await?;
        self.blocks.delete_many(&freed).await?;
        Ok(())
    }

    fn file_chunk_size(&self, storage: &Storage) -> u32 {
        match storage {
            Storage::Chunked { chunk_size } => *chunk_size,
            Storage::Inline(_) | Storage::None => self.options.chunk_size,
        }
    }

    fn ensure_file<'a>(&self, path: &str, meta: &'a InodeMeta) -> VfsResult<&'a InodeMeta> {
        match meta.kind {
            InodeType::File => Ok(meta),
            InodeType::Directory => Err(VfsError::eisdir(path)),
            InodeType::Symlink => Err(VfsError::eopnotsupp("resolved symlink without target file")),
            InodeType::CharacterDevice | InodeType::BlockDevice | InodeType::Fifo => {
                Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ))
            }
        }
    }

    async fn read_file_range(
        &self,
        meta: &InodeMeta,
        offset: u64,
        length: usize,
    ) -> VfsResult<Vec<u8>> {
        if length == 0 || offset >= meta.size {
            return Ok(Vec::new());
        }
        let available = meta.size.saturating_sub(offset).min(length as u64);
        let output_len = usize::try_from(available)
            .map_err(|_| VfsError::einval(format!("range length is too large: {available}")))?;

        match &meta.storage {
            Storage::Inline(data) => {
                let start = usize::try_from(offset).map_err(|_| {
                    VfsError::einval(format!("range offset is too large: {offset}"))
                })?;
                if start >= data.len() {
                    return Ok(vec![0; output_len]);
                }
                let end = start.saturating_add(output_len).min(data.len());
                let mut output = vec![0; output_len];
                output[..end - start].copy_from_slice(&data[start..end]);
                Ok(output)
            }
            Storage::None => Ok(vec![0; output_len]),
            Storage::Chunked { chunk_size } => {
                let chunk_size = u64::from(*chunk_size);
                let end_offset = offset
                    .checked_add(available)
                    .ok_or_else(|| VfsError::einval("range end overflows"))?;
                let start_index = offset / chunk_size;
                let end_index = end_offset.div_ceil(chunk_size);
                let chunks = self
                    .metadata
                    .get_chunks(
                        meta.ino,
                        ChunkRange {
                            start: start_index,
                            end: Some(end_index),
                        },
                    )
                    .await?;
                let mut output = vec![0; output_len];
                for chunk in chunks {
                    let chunk_start = chunk.index.saturating_mul(chunk_size);
                    let block = self.blocks.get(&chunk.key).await?;
                    let copy_start = offset.max(chunk_start);
                    let copy_end = end_offset.min(chunk_start.saturating_add(block.len() as u64));
                    if copy_start >= copy_end {
                        continue;
                    }
                    let output_start = usize::try_from(copy_start - offset)
                        .map_err(|_| VfsError::einval("range output offset is too large"))?;
                    let block_start = usize::try_from(copy_start - chunk_start)
                        .map_err(|_| VfsError::einval("range block offset is too large"))?;
                    let len = usize::try_from(copy_end - copy_start)
                        .map_err(|_| VfsError::einval("range copy length is too large"))?;
                    output[output_start..output_start + len]
                        .copy_from_slice(&block[block_start..block_start + len]);
                }
                Ok(output)
            }
        }
    }

    async fn put_chunk_edit(&self, index: u64, data: Vec<u8>) -> VfsResult<ChunkEdit> {
        let len = u32::try_from(data.len())
            .map_err(|_| VfsError::einval(format!("chunk is too large: {}", data.len())))?;
        let key = BlockKey::from_content(&data);
        if !self.blocks.exists(&key).await? {
            self.blocks.put(&key, &data).await?;
        }
        Ok(ChunkEdit { index, key, len })
    }

    async fn set_unwritten_extents(
        &self,
        meta: &InodeMeta,
        extents: &[(u64, u64)],
    ) -> VfsResult<()> {
        let mut xattrs = meta.xattrs.clone();
        encode_unwritten_extents(&mut xattrs, extents);
        let freed = self
            .metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    xattrs: Some(xattrs),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn write_chunked_range(
        &self,
        meta: &InodeMeta,
        content: &[u8],
        offset: u64,
    ) -> VfsResult<u64> {
        if content.is_empty() {
            return Ok(meta.size);
        }
        let content_len = u64::try_from(content.len()).map_err(|_| {
            VfsError::einval(format!("pwrite content is too large: {}", content.len()))
        })?;
        let end_offset = offset
            .checked_add(content_len)
            .ok_or_else(|| VfsError::einval("pwrite end offset overflows"))?;
        let new_size = meta.size.max(end_offset);

        if !matches!(meta.storage, Storage::Chunked { .. })
            && usize::try_from(new_size)
                .ok()
                .is_some_and(|len| len <= self.options.inline_threshold)
        {
            let old_len = usize::try_from(meta.size)
                .map_err(|_| VfsError::einval(format!("file is too large: {}", meta.size)))?;
            let mut data = self.read_file_range(meta, 0, old_len).await?;
            let start = usize::try_from(offset)
                .map_err(|_| VfsError::einval(format!("pwrite offset is too large: {offset}")))?;
            let end = start.saturating_add(content.len());
            if start > data.len() {
                data.resize(start, 0);
            }
            if end > data.len() {
                data.resize(end, 0);
            }
            data[start..end].copy_from_slice(content);
            let mut xattrs = meta.xattrs.clone();
            encode_unwritten_extents(
                &mut xattrs,
                &unwritten_after_write(
                    &decode_unwritten_extents(&meta.xattrs)?,
                    offset,
                    content_len,
                ),
            );
            let freed = self
                .metadata
                .set_attr(
                    meta.ino,
                    InodePatch {
                        storage: Some(Storage::Inline(data)),
                        size: Some(new_size),
                        allocated_extents: Some(allocation_after_write(
                            &meta.allocated_extents,
                            offset,
                            content_len,
                        )),
                        xattrs: Some(xattrs),
                        ..InodePatch::default()
                    },
                )
                .await?;
            self.blocks.delete_many(&freed).await?;
            return Ok(new_size);
        }

        let chunk_size = u64::from(match &meta.storage {
            Storage::Chunked { chunk_size } => *chunk_size,
            Storage::Inline(_) | Storage::None => self.initial_chunk_size(content.len()),
        });
        let start_index = offset / chunk_size;
        let end_index = end_offset.div_ceil(chunk_size);
        let existing_chunks = if matches!(meta.storage, Storage::Chunked { .. }) {
            self.metadata
                .get_chunks(
                    meta.ino,
                    ChunkRange {
                        start: start_index,
                        end: Some(end_index),
                    },
                )
                .await?
                .into_iter()
                .map(|chunk| (chunk.index, chunk.key))
                .collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };

        let mut edits = Vec::new();
        if let Storage::Inline(data) = &meta.storage {
            let inline_chunks = u64::try_from(data.len())
                .map_err(|_| VfsError::einval("inline data is too large"))?
                .div_ceil(chunk_size);
            for index in 0..inline_chunks.min(start_index) {
                let chunk_start = index.saturating_mul(chunk_size);
                let chunk_len = chunk_size.min(new_size.saturating_sub(chunk_start));
                let mut chunk_data = vec![
                    0;
                    usize::try_from(chunk_len).map_err(|_| {
                        VfsError::einval("inline prefix chunk is too large")
                    })?
                ];
                let copy_start = usize::try_from(chunk_start)
                    .map_err(|_| VfsError::einval("inline prefix offset is too large"))?;
                let copy_end = data.len().min(copy_start.saturating_add(chunk_data.len()));
                chunk_data[..copy_end - copy_start].copy_from_slice(&data[copy_start..copy_end]);
                edits.push(self.put_chunk_edit(index, chunk_data).await?);
            }
        }
        for index in start_index..end_index {
            let chunk_start = index.saturating_mul(chunk_size);
            let chunk_len = chunk_size.min(new_size.saturating_sub(chunk_start));
            let mut chunk_data = vec![
                0;
                usize::try_from(chunk_len).map_err(|_| {
                    VfsError::einval("chunk length is too large")
                })?
            ];

            match &meta.storage {
                Storage::Inline(data) => {
                    let copy_start = chunk_start.min(data.len() as u64);
                    let copy_end = chunk_start.saturating_add(chunk_len).min(data.len() as u64);
                    if copy_start < copy_end {
                        let dst = usize::try_from(copy_start - chunk_start)
                            .map_err(|_| VfsError::einval("inline chunk offset is too large"))?;
                        let src = usize::try_from(copy_start)
                            .map_err(|_| VfsError::einval("inline source offset is too large"))?;
                        let len = usize::try_from(copy_end - copy_start)
                            .map_err(|_| VfsError::einval("inline copy length is too large"))?;
                        chunk_data[dst..dst + len].copy_from_slice(&data[src..src + len]);
                    }
                }
                Storage::Chunked { .. } => {
                    if let Some(key) = existing_chunks.get(&index) {
                        let old = self.blocks.get(key).await?;
                        let len = old.len().min(chunk_data.len());
                        chunk_data[..len].copy_from_slice(&old[..len]);
                    }
                }
                Storage::None => {}
            }

            let write_start = offset.max(chunk_start);
            let write_end = end_offset.min(chunk_start.saturating_add(chunk_len));
            if write_start < write_end {
                let dst = usize::try_from(write_start - chunk_start)
                    .map_err(|_| VfsError::einval("chunk write offset is too large"))?;
                let src = usize::try_from(write_start - offset)
                    .map_err(|_| VfsError::einval("content write offset is too large"))?;
                let len = usize::try_from(write_end - write_start)
                    .map_err(|_| VfsError::einval("chunk write length is too large"))?;
                chunk_data[dst..dst + len].copy_from_slice(&content[src..src + len]);
            }

            edits.push(self.put_chunk_edit(index, chunk_data).await?);
        }

        if !matches!(meta.storage, Storage::Chunked { .. }) {
            self.metadata
                .set_attr(
                    meta.ino,
                    InodePatch {
                        storage: Some(Storage::Chunked {
                            chunk_size: u32::try_from(chunk_size)
                                .map_err(|_| VfsError::einval("chunk size is too large"))?,
                        }),
                        size: Some(new_size),
                        ..InodePatch::default()
                    },
                )
                .await?;
        }
        let freed = self
            .metadata
            .commit_write(
                meta.ino,
                edits,
                new_size,
                allocation_after_write(&meta.allocated_extents, offset, content_len),
            )
            .await?;
        self.blocks.delete_many(&freed).await?;
        self.set_unwritten_extents(
            meta,
            &unwritten_after_write(
                &decode_unwritten_extents(&meta.xattrs)?,
                offset,
                content_len,
            ),
        )
        .await?;
        Ok(new_size)
    }
}

#[async_trait]
impl<M: MetadataStore, B: BlockStore> VirtualFileSystem for ChunkedFs<M, B> {
    async fn read_file(&self, path: &str) -> VfsResult<Vec<u8>> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let len = usize::try_from(meta.size)
            .map_err(|_| VfsError::einval(format!("file is too large: {}", meta.size)))?;
        self.read_file_range(&meta, 0, len).await
    }

    async fn read_dir(&self, path: &str) -> VfsResult<Vec<String>> {
        let meta = self.metadata.resolve(path).await?;
        Ok(self
            .metadata
            .list_dir(meta.ino)
            .await?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    async fn read_dir_with_types(&self, path: &str) -> VfsResult<Vec<Dentry>> {
        let meta = self.metadata.resolve(path).await?;
        Ok(self
            .metadata
            .list_dir(meta.ino)
            .await?
            .into_iter()
            .map(|entry| Dentry {
                name: entry.name,
                ino: entry.meta.ino,
                kind: entry.meta.kind,
            })
            .collect())
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> VfsResult<()> {
        self.write_existing_or_create(path, content).await
    }

    async fn create_dir(&self, path: &str) -> VfsResult<()> {
        let (parent, name) = self.metadata.resolve_parent(path).await?;
        self.metadata
            .create(
                parent.ino,
                &name,
                CreateInodeAttrs::directory(
                    self.options.dir_mode,
                    self.options.uid,
                    self.options.gid,
                ),
            )
            .await?;
        Ok(())
    }

    async fn mkdir(&self, path: &str, recursive: bool) -> VfsResult<()> {
        if !recursive {
            return self.create_dir(path).await;
        }
        let normalized = normalize_path(path)?;
        let mut current = String::new();
        for part in normalized
            .trim_start_matches('/')
            .split('/')
            .filter(|p| !p.is_empty())
        {
            current.push('/');
            current.push_str(part);
            if !self.exists(&current).await {
                self.create_dir(&current).await?;
            }
        }
        Ok(())
    }

    async fn mknod(&self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let (parent, name) = self.metadata.resolve_parent(path).await?;
        self.metadata
            .create(
                parent.ino,
                &name,
                CreateInodeAttrs::special_node(mode, self.options.uid, self.options.gid, rdev)?,
            )
            .await?;
        Ok(())
    }

    async fn exists(&self, path: &str) -> bool {
        self.metadata.resolve(path).await.is_ok()
    }

    async fn stat(&self, path: &str) -> VfsResult<VirtualStat> {
        Ok(self.metadata.resolve(path).await?.to_stat())
    }

    async fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        Ok(self.metadata.lstat(path).await?.to_stat())
    }

    async fn remove_file(&self, path: &str) -> VfsResult<()> {
        let meta = self.metadata.lstat(path).await?;
        if meta.kind == InodeType::Directory {
            return Err(VfsError::eisdir(path));
        }
        let (parent, name) = self.metadata.resolve_parent(path).await?;
        let freed = self.metadata.remove(parent.ino, &name).await?;
        self.blocks.delete_many(&freed).await
    }

    async fn remove_dir(&self, path: &str) -> VfsResult<()> {
        let meta = self.metadata.lstat(path).await?;
        if meta.kind != InodeType::Directory {
            return Err(VfsError::enotdir(path));
        }
        let (parent, name) = self.metadata.resolve_parent(path).await?;
        let freed = self.metadata.remove(parent.ino, &name).await?;
        self.blocks.delete_many(&freed).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let (src_parent, src) = self.metadata.resolve_parent(old_path).await?;
        let (dst_parent, dst) = self.metadata.resolve_parent(new_path).await?;
        let freed = self
            .metadata
            .rename(src_parent.ino, &src, dst_parent.ino, &dst)
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn realpath(&self, path: &str) -> VfsResult<String> {
        self.metadata.resolve(path).await?;
        normalize_path(path)
    }

    async fn symlink(&self, target: &str, link_path: &str) -> VfsResult<()> {
        let (parent, name) = self.metadata.resolve_parent(link_path).await?;
        self.metadata
            .create(
                parent.ino,
                &name,
                CreateInodeAttrs::symlink(target.to_string(), self.options.uid, self.options.gid),
            )
            .await?;
        Ok(())
    }

    async fn readlink(&self, path: &str) -> VfsResult<String> {
        let meta = self.metadata.lstat(path).await?;
        if meta.kind != InodeType::Symlink {
            return Err(VfsError::einval(format!("not a symlink: {path}")));
        }
        Ok(meta.symlink_target.unwrap_or_default())
    }

    async fn link(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let target = self.metadata.resolve(old_path).await?;
        let (parent, name) = self.metadata.resolve_parent(new_path).await?;
        self.metadata.link(parent.ino, &name, target.ino).await
    }

    async fn chmod(&self, path: &str, mode: u32) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    mode: Some(mode),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    uid: Some(uid),
                    gid: Some(gid),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let meta = self.metadata.lstat(path).await?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    uid: Some(uid),
                    gid: Some(gid),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn get_xattr(&self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        validate_xattr_name(name)?;
        let meta = if follow_symlinks {
            self.metadata.resolve(path).await?
        } else {
            self.metadata.lstat(path).await?
        };
        meta.xattrs.get(name).cloned().ok_or_else(|| {
            VfsError::new(
                "ENODATA",
                format!("extended attribute does not exist: {name}"),
            )
        })
    }

    async fn list_xattrs(&self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let meta = if follow_symlinks {
            self.metadata.resolve(path).await?
        } else {
            self.metadata.lstat(path).await?
        };
        Ok(meta
            .xattrs
            .into_keys()
            .filter(|name| !name.starts_with(INTERNAL_XATTR_PREFIX))
            .collect())
    }

    async fn set_xattr(
        &self,
        path: &str,
        name: &str,
        value: &[u8],
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let meta = if follow_symlinks {
            self.metadata.resolve(path).await?
        } else {
            self.metadata.lstat(path).await?
        };
        let mut xattrs = meta.xattrs;
        set_xattr_value(&mut xattrs, name, value, flags)?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    xattrs: Some(xattrs),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn remove_xattr(&self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        validate_xattr_name(name)?;
        let meta = if follow_symlinks {
            self.metadata.resolve(path).await?
        } else {
            self.metadata.lstat(path).await?
        };
        let mut xattrs = meta.xattrs;
        if xattrs.remove(name).is_none() {
            return Err(VfsError::new(
                "ENODATA",
                format!("extended attribute does not exist: {name}"),
            ));
        }
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    xattrs: Some(xattrs),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn utimes(&self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    atime: Some(ms_to_timespec(atime_ms)),
                    mtime: Some(ms_to_timespec(mtime_ms)),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn set_atime(&self, path: &str, atime_ms: u64) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    atime: Some(ms_to_timespec(atime_ms)),
                    ..InodePatch::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn truncate(&self, path: &str, length: u64) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let next_allocated_extents = if length <= meta.size {
            allocation_after_truncate(&meta.allocated_extents, length)
        } else {
            meta.allocated_extents.clone()
        };
        let next_unwritten_extents = if length <= meta.size {
            unwritten_after_truncate(&decode_unwritten_extents(&meta.xattrs)?, length)
        } else {
            decode_unwritten_extents(&meta.xattrs)?
        };

        if usize::try_from(length)
            .ok()
            .is_some_and(|len| len <= self.options.inline_threshold)
        {
            let data = self
                .read_file_range(&meta, 0, usize::try_from(length).unwrap_or(0))
                .await?;
            let mut xattrs = meta.xattrs.clone();
            encode_unwritten_extents(&mut xattrs, &next_unwritten_extents);
            let freed = self
                .metadata
                .set_attr(
                    meta.ino,
                    InodePatch {
                        storage: Some(Storage::Inline(data)),
                        size: Some(length),
                        allocated_extents: Some(next_allocated_extents),
                        xattrs: Some(xattrs),
                        ..InodePatch::default()
                    },
                )
                .await?;
            self.blocks.delete_many(&freed).await?;
            return Ok(());
        }

        let chunk_size = u64::from(self.file_chunk_size(&meta.storage));
        let mut edits = Vec::new();
        if !matches!(meta.storage, Storage::Chunked { .. }) {
            let existing_len = meta.size.min(length);
            let mut offset = 0;
            while offset < existing_len {
                let len = (existing_len - offset).min(chunk_size);
                let data = self
                    .read_file_range(
                        &meta,
                        offset,
                        usize::try_from(len)
                            .map_err(|_| VfsError::einval("truncate chunk is too large"))?,
                    )
                    .await?;
                edits.push(self.put_chunk_edit(offset / chunk_size, data).await?);
                offset = offset.saturating_add(len);
            }
            self.metadata
                .set_attr(
                    meta.ino,
                    InodePatch {
                        storage: Some(Storage::Chunked {
                            chunk_size: self.options.chunk_size,
                        }),
                        size: Some(length),
                        ..InodePatch::default()
                    },
                )
                .await?;
        } else if length < meta.size && !length.is_multiple_of(chunk_size) {
            let final_index = length / chunk_size;
            let final_start = final_index.saturating_mul(chunk_size);
            let final_len = length - final_start;
            let data = self
                .read_file_range(
                    &meta,
                    final_start,
                    usize::try_from(final_len)
                        .map_err(|_| VfsError::einval("truncate final chunk is too large"))?,
                )
                .await?;
            edits.push(self.put_chunk_edit(final_index, data).await?);
        }

        let freed = self
            .metadata
            .commit_write(meta.ino, edits, length, next_allocated_extents)
            .await?;
        self.blocks.delete_many(&freed).await?;
        self.set_unwritten_extents(&meta, &next_unwritten_extents)
            .await
    }

    async fn allocate(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("allocation range overflows"))?;
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let mut xattrs = meta.xattrs.clone();
        encode_unwritten_extents(
            &mut xattrs,
            &unwritten_after_allocate(
                &decode_unwritten_extents(&meta.xattrs)?,
                &meta.allocated_extents,
                offset,
                length,
            ),
        );
        let freed = self
            .metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    size: Some(meta.size.max(end)),
                    allocated_extents: Some(allocation_after_write(
                        &meta.allocated_extents,
                        offset,
                        length,
                    )),
                    xattrs: Some(xattrs),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn punch_hole(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        const PUNCH_CHUNK_BYTES: u64 = 64 * 1024;

        let requested_end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("hole-punch range overflows"))?;
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let end = requested_end.min(meta.size);
        let mut cursor = offset.min(meta.size);
        while cursor < end {
            let chunk_len = usize::try_from((end - cursor).min(PUNCH_CHUNK_BYTES))
                .map_err(|_| VfsError::einval("hole-punch chunk is too large"))?;
            self.write_chunked_range(&meta, &vec![0; chunk_len], cursor)
                .await?;
            cursor += chunk_len as u64;
        }
        let freed = self
            .metadata
            .set_attr(
                meta.ino,
                InodePatch {
                    allocated_extents: Some(allocation_after_punch(
                        &meta.allocated_extents,
                        offset,
                        length,
                    )),
                    xattrs: Some({
                        let mut xattrs = meta.xattrs.clone();
                        encode_unwritten_extents(
                            &mut xattrs,
                            &unwritten_after_write(
                                &decode_unwritten_extents(&meta.xattrs)?,
                                offset,
                                length,
                            ),
                        );
                        xattrs
                    }),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn zero_range(
        &self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        const ZERO_CHUNK_BYTES: u64 = 64 * 1024;

        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("zero range overflows"))?;
        if length == 0 {
            return Err(VfsError::einval("zero range length must be nonzero"));
        }
        let original = self.metadata.resolve(path).await?;
        self.ensure_file(path, &original)?;
        let zero_end = if keep_size {
            end.min(original.size)
        } else {
            end
        };
        let mut cursor = offset.min(zero_end);
        while cursor < zero_end {
            let chunk_len = usize::try_from((zero_end - cursor).min(ZERO_CHUNK_BYTES))
                .map_err(|_| VfsError::einval("zero range chunk is too large"))?;
            self.pwrite(path, &vec![0; chunk_len], cursor).await?;
            cursor += chunk_len as u64;
        }
        let updated = self.metadata.resolve(path).await?;
        let freed = self
            .metadata
            .set_attr(
                updated.ino,
                InodePatch {
                    size: keep_size.then_some(original.size),
                    allocated_extents: Some(allocation_after_write(
                        &updated.allocated_extents,
                        offset,
                        length,
                    )),
                    xattrs: Some({
                        let mut xattrs = original.xattrs.clone();
                        encode_unwritten_extents(
                            &mut xattrs,
                            &unwritten_after_zero(
                                &decode_unwritten_extents(&original.xattrs)?,
                                &original.allocated_extents,
                                offset,
                                length,
                            ),
                        );
                        xattrs
                    }),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn insert_range(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        if offset >= meta.size {
            return Err(VfsError::einval("insert range offset must be before EOF"));
        }
        let mut data = self
            .read_file_range(
                &meta,
                0,
                usize::try_from(meta.size)
                    .map_err(|_| VfsError::einval("insert source is too large"))?,
            )
            .await?;
        let start = usize::try_from(offset)
            .map_err(|_| VfsError::einval("insert range offset is too large"))?;
        let insert_len = usize::try_from(length)
            .map_err(|_| VfsError::einval("insert range length is too large"))?;
        data.splice(start..start, std::iter::repeat_n(0, insert_len));
        let extents = allocation_after_insert(&meta.allocated_extents, offset, length);
        let unwritten =
            unwritten_after_insert(&decode_unwritten_extents(&meta.xattrs)?, offset, length);
        self.write_existing_or_create(path, &data).await?;
        let updated = self.metadata.resolve(path).await?;
        let freed = self
            .metadata
            .set_attr(
                updated.ino,
                InodePatch {
                    allocated_extents: Some(extents),
                    xattrs: Some({
                        let mut xattrs = meta.xattrs.clone();
                        encode_unwritten_extents(&mut xattrs, &unwritten);
                        xattrs
                    }),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn collapse_range(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("collapse range overflows"))?;
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        if end >= meta.size {
            return Err(VfsError::einval("collapse range must end before EOF"));
        }
        let mut data = self
            .read_file_range(
                &meta,
                0,
                usize::try_from(meta.size)
                    .map_err(|_| VfsError::einval("collapse source is too large"))?,
            )
            .await?;
        let start = usize::try_from(offset)
            .map_err(|_| VfsError::einval("collapse range offset is too large"))?;
        let end = usize::try_from(end)
            .map_err(|_| VfsError::einval("collapse range end is too large"))?;
        data.drain(start..end);
        let extents = allocation_after_collapse(&meta.allocated_extents, offset, length);
        let unwritten =
            unwritten_after_collapse(&decode_unwritten_extents(&meta.xattrs)?, offset, length);
        self.write_existing_or_create(path, &data).await?;
        let updated = self.metadata.resolve(path).await?;
        let freed = self
            .metadata
            .set_attr(
                updated.ino,
                InodePatch {
                    allocated_extents: Some(extents),
                    xattrs: Some({
                        let mut xattrs = meta.xattrs.clone();
                        encode_unwritten_extents(&mut xattrs, &unwritten);
                        xattrs
                    }),
                    ..InodePatch::default()
                },
            )
            .await?;
        self.blocks.delete_many(&freed).await
    }

    async fn allocated_ranges(&self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let unwritten = unwritten_sector_ranges(&meta.xattrs)?;
        let extent_limit = allocation_limit(unwritten, meta.size);
        Ok(allocation_byte_ranges(
            &meta.allocated_extents,
            extent_limit,
        ))
    }

    async fn unwritten_ranges(&self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let unwritten = unwritten_sector_ranges(&meta.xattrs)?;
        let extent_limit = allocation_limit(unwritten, meta.size);
        unwritten_byte_ranges(&meta.xattrs, extent_limit)
    }

    async fn extent_at(&self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let unwritten = unwritten_sector_ranges(&meta.xattrs)?;
        let extent_limit = allocation_limit(unwritten.clone(), meta.size);
        let extent = crate::extent::classified_file_extent_at(
            crate::extent::sector_byte_ranges(meta.allocated_extents.iter().copied(), extent_limit),
            crate::extent::sector_byte_ranges(unwritten, extent_limit),
            index,
        );
        Ok(extent.map(|extent| FileExtent {
            start: extent.start,
            end: extent.end,
            unwritten: extent.unwritten,
        }))
    }

    async fn pread(&self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        self.read_file_range(&meta, offset, length).await
    }

    async fn pwrite(&self, path: &str, content: &[u8], offset: u64) -> VfsResult<()> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        self.write_chunked_range(&meta, content, offset).await?;
        Ok(())
    }

    async fn append(&self, path: &str, content: &[u8]) -> VfsResult<u64> {
        let meta = self.metadata.resolve(path).await?;
        self.ensure_file(path, &meta)?;
        let len = meta
            .size
            .checked_add(u64::try_from(content.len()).map_err(|_| {
                VfsError::einval(format!("append content is too large: {}", content.len()))
            })?)
            .ok_or_else(|| VfsError::einval("append size overflows"))?;
        self.write_chunked_range(&meta, content, meta.size).await?;
        Ok(len)
    }

    async fn sync(&self, _path: &str) -> VfsResult<()> {
        self.blocks.sync().await?;
        self.metadata.flush().await
    }
}

#[async_trait]
impl<M: MetadataStore, B: BlockStore> Snapshottable for ChunkedFs<M, B> {
    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId> {
        self.metadata.snapshot(root).await
    }

    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64> {
        self.metadata.fork(snap).await
    }
}

fn ms_to_timespec(ms: u64) -> Timespec {
    Timespec {
        sec: (ms / 1_000) as i64,
        nsec: ((ms % 1_000) * 1_000_000) as u32,
    }
}

fn dense_allocation(size: u64) -> Vec<(u64, u64)> {
    if size == 0 {
        Vec::new()
    } else {
        vec![(0, size.div_ceil(512))]
    }
}

fn allocation_after_write(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    if length == 0 {
        return existing.to_vec();
    }
    let mut pending = (offset / 512, offset.saturating_add(length).div_ceil(512));
    let mut merged = Vec::with_capacity(existing.len() + 1);
    for &(start, end) in existing {
        if end < pending.0 {
            merged.push((start, end));
        } else if pending.1 < start {
            merged.push(pending);
            pending = (start, end);
        } else {
            pending.0 = pending.0.min(start);
            pending.1 = pending.1.max(end);
        }
    }
    merged.push(pending);
    merged
}

fn allocation_after_truncate(existing: &[(u64, u64)], size: u64) -> Vec<(u64, u64)> {
    let end = size.div_ceil(512);
    existing
        .iter()
        .filter_map(|(start, extent_end)| {
            let extent_end = (*extent_end).min(end);
            (*start < extent_end).then_some((*start, extent_end))
        })
        .collect()
}

fn allocation_after_punch(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    let start = offset.div_ceil(512);
    let end = offset.saturating_add(length) / 512;
    if start >= end {
        return existing.to_vec();
    }
    existing
        .iter()
        .flat_map(|&(extent_start, extent_end)| {
            [
                (extent_start, extent_end.min(start)),
                (extent_start.max(end), extent_end),
            ]
            .into_iter()
            .filter(|(part_start, part_end)| part_start < part_end)
        })
        .collect()
}

fn validate_shift_range(offset: u64, length: u64) -> VfsResult<()> {
    if length == 0 || !offset.is_multiple_of(512) || !length.is_multiple_of(512) {
        return Err(VfsError::einval(
            "insert/collapse range requires a nonzero 512-byte-aligned range",
        ));
    }
    Ok(())
}

fn allocation_after_insert(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let shift = length / 512;
    normalize_extents(existing.iter().flat_map(|&(extent_start, extent_end)| {
        if extent_end <= start {
            vec![(extent_start, extent_end)]
        } else if extent_start >= start {
            vec![(extent_start + shift, extent_end + shift)]
        } else {
            vec![(extent_start, start), (start + shift, extent_end + shift)]
        }
    }))
}

fn allocation_after_collapse(existing: &[(u64, u64)], offset: u64, length: u64) -> Vec<(u64, u64)> {
    let start = offset / 512;
    let end = start + length / 512;
    normalize_extents(existing.iter().flat_map(|&(extent_start, extent_end)| {
        let mut parts = Vec::with_capacity(2);
        if extent_start < start {
            parts.push((extent_start, extent_end.min(start)));
        }
        if extent_end > end {
            parts.push((
                extent_start.max(end) - (end - start),
                extent_end - (end - start),
            ));
        }
        parts
    }))
}

fn normalize_extents(extents: impl IntoIterator<Item = (u64, u64)>) -> Vec<(u64, u64)> {
    let mut merged = Vec::<(u64, u64)>::new();
    for (start, end) in extents.into_iter().filter(|(start, end)| start < end) {
        if let Some(last) = merged.last_mut().filter(|last| start <= last.1) {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn allocation_limit(extents: impl Iterator<Item = (u64, u64)>, size: u64) -> u64 {
    extents
        .last()
        .map_or(size, |(_, end)| size.max(end.saturating_mul(512)))
}

fn allocation_byte_ranges(extents: &[(u64, u64)], limit: u64) -> Vec<(u64, u64)> {
    extents
        .iter()
        .filter_map(|&(start, end)| {
            let start = start.saturating_mul(512).min(limit);
            let end = end.saturating_mul(512).min(limit);
            (start < end).then_some((start, end))
        })
        .collect()
}
