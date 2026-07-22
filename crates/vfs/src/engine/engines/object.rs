use crate::engine::block::ObjectBackend;
use crate::engine::error::{VfsError, VfsResult};
use crate::engine::types::{
    inode_rdev, normalize_path, set_xattr_value, validate_xattr_name, Dentry, InodeType,
    ObjectMeta, Timespec, VirtualStat, INODE_RDEV_XATTR, INTERNAL_XATTR_PREFIX, S_IFBLK, S_IFCHR,
    S_IFIFO,
};
use crate::engine::vfs::VirtualFileSystem;
use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct ObjectFsOptions {
    pub prefix: String,
    pub uid: u32,
    pub gid: u32,
    pub file_mode: u32,
    pub dir_mode: u32,
}

impl Default for ObjectFsOptions {
    fn default() -> Self {
        Self {
            prefix: String::new(),
            uid: 0,
            gid: 0,
            file_mode: 0o644,
            dir_mode: 0o755,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectFs<B> {
    backend: B,
    options: ObjectFsOptions,
}

impl<B> ObjectFs<B> {
    pub fn new(backend: B) -> Self {
        Self::with_options(backend, ObjectFsOptions::default())
    }

    pub fn with_options(backend: B, options: ObjectFsOptions) -> Self {
        Self { backend, options }
    }

    fn key_for(&self, path: &str) -> VfsResult<String> {
        let normalized = normalize_path(path)?;
        let relative = normalized.trim_start_matches('/');
        Ok(format!("{}{}", self.options.prefix, relative))
    }

    fn dir_prefix_for(&self, path: &str) -> VfsResult<String> {
        let mut key = self.key_for(path)?;
        if !key.is_empty() && !key.ends_with('/') {
            key.push('/');
        }
        Ok(key)
    }

    fn file_meta(&self, size: u64) -> ObjectMeta {
        let now = Timespec::now();
        ObjectMeta {
            size,
            allocated_extents: (size > 0).then_some((0, size)).into_iter().collect(),
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            mode: self.options.file_mode,
            uid: self.options.uid,
            gid: self.options.gid,
            kind: InodeType::File,
            symlink_target: None,
            link_id: None,
            xattrs: Default::default(),
        }
    }

    fn dir_meta(&self) -> ObjectMeta {
        let now = Timespec::now();
        ObjectMeta {
            size: 0,
            allocated_extents: Vec::new(),
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            mode: self.options.dir_mode,
            uid: self.options.uid,
            gid: self.options.gid,
            kind: InodeType::Directory,
            symlink_target: None,
            link_id: None,
            xattrs: Default::default(),
        }
    }
}

impl<B: ObjectBackend> ObjectFs<B> {
    async fn object_for_path(&self, path: &str) -> VfsResult<(String, ObjectMeta)> {
        let key = self.key_for(path)?;
        if let Some(meta) = self.backend.head(&key).await? {
            return Ok((key, meta));
        }
        let directory_key = self.dir_prefix_for(path)?;
        if directory_key != key {
            if let Some(meta) = self.backend.head(&directory_key).await? {
                return Ok((directory_key, meta));
            }
        }
        Err(VfsError::enoent(path))
    }

    async fn rewrite_metadata<F>(&self, path: &str, update: F) -> VfsResult<()>
    where
        F: FnOnce(&mut ObjectMeta) -> VfsResult<()> + Send,
    {
        let (key, mut meta) = self.object_for_path(path).await?;
        let contents = self.backend.get_range(&key, 0, meta.size).await?;
        update(&mut meta)?;
        self.backend.put(&key, &contents, meta).await
    }

    async fn collect_objects_under(&self, prefix: &str) -> VfsResult<Vec<String>> {
        let mut pending = vec![prefix.to_string()];
        let mut objects = Vec::new();
        while let Some(current) = pending.pop() {
            for entry in self.backend.list(&current).await? {
                if entry.is_prefix {
                    pending.push(entry.name);
                } else {
                    objects.push(entry.name);
                }
            }
        }
        Ok(objects)
    }
}

#[async_trait]
impl<B: ObjectBackend> VirtualFileSystem for ObjectFs<B> {
    async fn read_file(&self, path: &str) -> VfsResult<Vec<u8>> {
        let key = self.key_for(path)?;
        let meta = self
            .backend
            .head(&key)
            .await?
            .ok_or_else(|| VfsError::enoent(path))?;
        if meta.kind == InodeType::Directory {
            return Err(VfsError::eisdir(path));
        }
        if meta.kind == InodeType::Symlink {
            let target = meta.symlink_target.ok_or_else(|| VfsError::enoent(path))?;
            return self.read_file(&target).await;
        }
        if matches!(
            meta.kind,
            InodeType::CharacterDevice | InodeType::BlockDevice | InodeType::Fifo
        ) {
            return Err(VfsError::new(
                "ENXIO",
                format!("device I/O requires kernel dispatch: {path}"),
            ));
        }
        self.backend.get_range(&key, 0, meta.size).await
    }

    async fn read_dir(&self, path: &str) -> VfsResult<Vec<String>> {
        Ok(self
            .read_dir_with_types(path)
            .await?
            .into_iter()
            .map(|entry| entry.name)
            .collect())
    }

    async fn read_dir_with_types(&self, path: &str) -> VfsResult<Vec<Dentry>> {
        let prefix = self.dir_prefix_for(path)?;
        let entries = self.backend.list(&prefix).await?;
        let mut result = Vec::new();
        for entry in entries {
            let name = entry
                .name
                .trim_start_matches(&prefix)
                .trim_end_matches('/')
                .to_string();
            if name.is_empty() || name.contains('/') {
                continue;
            }
            result.push(Dentry {
                name,
                ino: object_ino(&entry.name),
                kind: match self.backend.head(&entry.name).await? {
                    Some(meta) => meta.kind,
                    None if entry.is_prefix => InodeType::Directory,
                    None => InodeType::File,
                },
            });
        }
        Ok(result)
    }

    async fn write_file(&self, path: &str, content: &[u8]) -> VfsResult<()> {
        let key = self.key_for(path)?;
        let meta = match self.backend.head(&key).await? {
            Some(mut meta) if meta.kind == InodeType::File => {
                let now = Timespec::now();
                meta.size = content.len() as u64;
                meta.allocated_extents = (!content.is_empty())
                    .then_some((0, content.len() as u64))
                    .into_iter()
                    .collect();
                meta.mtime = now;
                meta.ctime = now;
                meta
            }
            Some(meta) if meta.kind == InodeType::Directory => {
                return Err(VfsError::eisdir(path));
            }
            Some(_) => {
                return Err(VfsError::new(
                    "ENXIO",
                    format!("device I/O requires kernel dispatch: {path}"),
                ));
            }
            None => self.file_meta(content.len() as u64),
        };
        self.backend.put(&key, content, meta).await
    }

    async fn create_dir(&self, path: &str) -> VfsResult<()> {
        let key = self.dir_prefix_for(path)?;
        self.backend.put(&key, &[], self.dir_meta()).await
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
            self.create_dir(&current).await?;
        }
        Ok(())
    }

    async fn mknod(&self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let kind = match mode & 0o170000 {
            S_IFCHR => InodeType::CharacterDevice,
            S_IFBLK => InodeType::BlockDevice,
            S_IFIFO => InodeType::Fifo,
            _ => return Err(VfsError::einval("unsupported special inode type")),
        };
        let now = Timespec::now();
        let mut meta = ObjectMeta {
            size: 0,
            allocated_extents: Vec::new(),
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            mode: mode & 0o7777,
            uid: self.options.uid,
            gid: self.options.gid,
            kind,
            symlink_target: None,
            link_id: None,
            xattrs: Default::default(),
        };
        if matches!(kind, InodeType::CharacterDevice | InodeType::BlockDevice) {
            meta.xattrs
                .insert(String::from(INODE_RDEV_XATTR), rdev.to_le_bytes().to_vec());
        }
        self.backend.put(&self.key_for(path)?, &[], meta).await
    }

    async fn exists(&self, path: &str) -> bool {
        let Ok(key) = self.key_for(path) else {
            return false;
        };
        if self.backend.head(&key).await.ok().flatten().is_some() {
            return true;
        }
        let Ok(prefix) = self.dir_prefix_for(path) else {
            return false;
        };
        self.backend
            .list(&prefix)
            .await
            .map(|entries| !entries.is_empty())
            .unwrap_or(false)
    }

    async fn stat(&self, path: &str) -> VfsResult<VirtualStat> {
        match self.object_for_path(path).await {
            Ok((key, meta)) => return Ok(object_stat(meta, &key)),
            Err(error) if error.code() == "ENOENT" => {}
            Err(error) => return Err(error),
        }
        let key = self.key_for(path)?;
        let entries = self.backend.list(&self.dir_prefix_for(path)?).await?;
        if entries.is_empty() {
            return Err(VfsError::enoent(path));
        }
        Ok(object_stat(self.dir_meta(), &key))
    }

    async fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.stat(path).await
    }

    async fn remove_file(&self, path: &str) -> VfsResult<()> {
        self.backend.delete(&self.key_for(path)?).await
    }

    async fn remove_dir(&self, path: &str) -> VfsResult<()> {
        let prefix = self.dir_prefix_for(path)?;
        let entries = self.backend.list(&prefix).await?;
        if entries.iter().any(|entry| entry.name != prefix) {
            return Err(VfsError::enotempty(path));
        }
        self.backend.delete(&prefix).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_key = self.key_for(old_path)?;
        if self.backend.head(&old_key).await?.is_some() {
            let new_key = self.key_for(new_path)?;
            self.backend.copy(&old_key, &new_key).await?;
            self.backend.delete(&old_key).await?;
            return Ok(());
        }
        let old_prefix = self.dir_prefix_for(old_path)?;
        let new_prefix = self.dir_prefix_for(new_path)?;
        let objects = self.collect_objects_under(&old_prefix).await?;
        if objects.is_empty() {
            return Err(VfsError::enoent(old_path));
        }
        for key in &objects {
            let dst = format!("{new_prefix}{}", key.trim_start_matches(&old_prefix));
            self.backend.copy(key, &dst).await?;
        }
        for key in objects {
            self.backend.delete(&key).await?;
        }
        Ok(())
    }

    async fn realpath(&self, path: &str) -> VfsResult<String> {
        if !self.exists(path).await {
            return Err(VfsError::enoent(path));
        }
        normalize_path(path)
    }

    async fn symlink(&self, target: &str, link_path: &str) -> VfsResult<()> {
        let key = self.key_for(link_path)?;
        let now = Timespec::now();
        let meta = ObjectMeta {
            size: 0,
            allocated_extents: Vec::new(),
            atime: now,
            mtime: now,
            ctime: now,
            birthtime: now,
            mode: 0o777,
            uid: self.options.uid,
            gid: self.options.gid,
            kind: InodeType::Symlink,
            symlink_target: Some(target.to_string()),
            link_id: None,
            xattrs: Default::default(),
        };
        self.backend.put(&key, &[], meta).await
    }

    async fn readlink(&self, path: &str) -> VfsResult<String> {
        let key = self.key_for(path)?;
        let meta = self
            .backend
            .head(&key)
            .await?
            .ok_or_else(|| VfsError::enoent(path))?;
        if meta.kind != InodeType::Symlink {
            return Err(VfsError::einval(format!("not a symlink: {path}")));
        }
        Ok(meta.symlink_target.unwrap_or_default())
    }

    async fn link(&self, _old_path: &str, _new_path: &str) -> VfsResult<()> {
        Err(VfsError::eopnotsupp("ObjectFs does not support hard links"))
    }

    async fn chmod(&self, path: &str, mode: u32) -> VfsResult<()> {
        self.rewrite_metadata(path, |meta| {
            meta.mode = mode & 0o7777;
            meta.ctime = Timespec::now();
            Ok(())
        })
        .await
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.rewrite_metadata(path, |meta| {
            meta.uid = uid;
            meta.gid = gid;
            meta.ctime = Timespec::now();
            Ok(())
        })
        .await
    }

    async fn get_xattr(
        &self,
        path: &str,
        name: &str,
        _follow_symlinks: bool,
    ) -> VfsResult<Vec<u8>> {
        validate_xattr_name(name)?;
        let (_, meta) = self.object_for_path(path).await?;
        meta.xattrs.get(name).cloned().ok_or_else(|| {
            VfsError::new(
                "ENODATA",
                format!("extended attribute does not exist: {name}"),
            )
        })
    }

    async fn list_xattrs(&self, path: &str, _follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let (_, meta) = self.object_for_path(path).await?;
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
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        validate_xattr_name(name)?;
        self.rewrite_metadata(path, |meta| {
            set_xattr_value(&mut meta.xattrs, name, value, flags)?;
            meta.ctime = Timespec::now();
            Ok(())
        })
        .await
    }

    async fn remove_xattr(&self, path: &str, name: &str, _follow_symlinks: bool) -> VfsResult<()> {
        validate_xattr_name(name)?;
        self.rewrite_metadata(path, |meta| {
            if meta.xattrs.remove(name).is_none() {
                return Err(VfsError::new(
                    "ENODATA",
                    format!("extended attribute does not exist: {name}"),
                ));
            }
            meta.ctime = Timespec::now();
            Ok(())
        })
        .await
    }

    async fn utimes(&self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        self.rewrite_metadata(path, |meta| {
            meta.atime = ms_to_timespec(atime_ms);
            meta.mtime = ms_to_timespec(mtime_ms);
            meta.ctime = Timespec::now();
            Ok(())
        })
        .await
    }

    async fn set_atime(&self, path: &str, atime_ms: u64) -> VfsResult<()> {
        self.rewrite_metadata(path, |meta| {
            meta.atime = ms_to_timespec(atime_ms);
            Ok(())
        })
        .await
    }

    async fn truncate(&self, path: &str, length: u64) -> VfsResult<()> {
        let mut data = self.read_file(path).await?;
        let length = usize::try_from(length)
            .map_err(|_| VfsError::einval(format!("truncate length is too large: {length}")))?;
        data.resize(length, 0);
        self.write_file(path, &data).await
    }

    async fn pread(&self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let key = self.key_for(path)?;
        self.backend.get_range(&key, offset, length as u64).await
    }

    async fn pwrite(&self, path: &str, content: &[u8], offset: u64) -> VfsResult<()> {
        let mut data = self.read_file(path).await?;
        let start = usize::try_from(offset)
            .map_err(|_| VfsError::einval(format!("pwrite offset is too large: {offset}")))?;
        if start > data.len() {
            data.resize(start, 0);
        }
        let end = start.saturating_add(content.len());
        if end > data.len() {
            data.resize(end, 0);
        }
        data[start..end].copy_from_slice(content);
        self.write_file(path, &data).await
    }

    async fn append(&self, path: &str, content: &[u8]) -> VfsResult<u64> {
        let mut data = self.read_file(path).await?;
        data.extend_from_slice(content);
        let len = data.len() as u64;
        self.write_file(path, &data).await?;
        Ok(len)
    }
}

fn object_ino(key: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in key.trim_end_matches('/').as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if hash == 0 {
        1
    } else {
        hash
    }
}

fn object_stat(meta: ObjectMeta, key: &str) -> VirtualStat {
    let type_bits = match meta.kind {
        InodeType::File => crate::engine::types::S_IFREG,
        InodeType::Directory => crate::engine::types::S_IFDIR,
        InodeType::Symlink => crate::engine::types::S_IFLNK,
        InodeType::CharacterDevice => crate::engine::types::S_IFCHR,
        InodeType::BlockDevice => crate::engine::types::S_IFBLK,
        InodeType::Fifo => crate::engine::types::S_IFIFO,
    };
    VirtualStat {
        mode: type_bits | (meta.mode & 0o7777),
        size: meta.size,
        blocks: meta.size.div_ceil(512),
        rdev: inode_rdev(&meta.xattrs),
        is_directory: meta.kind == InodeType::Directory,
        is_symbolic_link: meta.kind == InodeType::Symlink,
        atime: meta.atime,
        mtime: meta.mtime,
        ctime: meta.ctime,
        birthtime: meta.birthtime,
        ino: object_ino(key),
        nlink: 1,
        uid: meta.uid,
        gid: meta.gid,
    }
}

fn ms_to_timespec(ms: u64) -> Timespec {
    Timespec {
        sec: (ms / 1_000) as i64,
        nsec: ((ms % 1_000) * 1_000_000) as u32,
    }
}
