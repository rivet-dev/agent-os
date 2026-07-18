use super::root_fs::RootFileSystem;
use super::usage::{FileSystemStats, FileSystemUsage};
use super::vfs::{
    FileExtent, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualUtimeSpec,
};
use std::any::Any;
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};
use web_time::{SystemTime, UNIX_EPOCH};

const MAX_REALPATH_SYMLINKS: usize = 40;

pub trait MountedFileSystem: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>>;
    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>>;
    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        let entries = self.read_dir(path)?;
        if entries.len() > max_entries {
            return Err(VfsError::new(
                "ENOMEM",
                format!(
                    "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                ),
            ));
        }
        Ok(entries)
    }
    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>>;
    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()>;
    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let _ = mode;
        self.write_file(path, content)
    }
    fn create_file_exclusive(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        if self.exists(path) {
            return Err(VfsError::new(
                "EEXIST",
                format!("file already exists, open '{path}'"),
            ));
        }
        self.write_file(path, content)
    }
    fn create_file_exclusive_with_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let _ = mode;
        self.create_file_exclusive(path, content)
    }
    fn append_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<u64> {
        let mut existing = self.read_file(path)?;
        existing.extend_from_slice(&content);
        let new_len = existing.len() as u64;
        self.write_file(path, existing)?;
        Ok(new_len)
    }
    fn create_dir(&mut self, path: &str) -> VfsResult<()>;
    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> VfsResult<()> {
        let _ = mode;
        self.create_dir(path)
    }
    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()>;
    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let _ = (mode, rdev);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("special inode creation is not supported for mount path '{path}'"),
        ))
    }
    fn mkdir_with_mode(&mut self, path: &str, recursive: bool, mode: Option<u32>) -> VfsResult<()> {
        let _ = mode;
        self.mkdir(path, recursive)
    }
    fn exists(&self, path: &str) -> bool;
    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat>;
    fn remove_file(&mut self, path: &str) -> VfsResult<()>;
    fn remove_dir(&mut self, path: &str) -> VfsResult<()>;
    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn realpath(&self, path: &str) -> VfsResult<String>;
    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()>;
    fn read_link(&self, path: &str) -> VfsResult<String>;
    fn lstat(&self, path: &str) -> VfsResult<VirtualStat>;
    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()>;
    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()>;
    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()>;
    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if !follow_symlinks {
            return Err(VfsError::unsupported(format!(
                "lchown is not supported for mount path '{path}'"
            )));
        }
        self.chown(path, uid, gid)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.chown(path, uid, gid)
    }
    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        let _ = (name, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for mount path '{path}'"),
        ))
    }
    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let _ = follow_symlinks;
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for mount path '{path}'"),
        ))
    }
    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let _ = (name, value, flags, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for mount path '{path}'"),
        ))
    }
    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        let _ = (name, follow_symlinks);
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extended attributes are not supported for mount path '{path}'"),
        ))
    }
    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()>;
    fn set_atime(&mut self, path: &str, atime_ms: u64) -> VfsResult<()> {
        let mtime_ms = self.stat(path)?.mtime_ms;
        self.utimes(path, atime_ms, mtime_ms)
    }
    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if !follow_symlinks {
            return Err(VfsError::unsupported(format!(
                "lutimes is not supported for mount path '{path}'"
            )));
        }
        let existing = match (atime, mtime) {
            (VirtualUtimeSpec::Omit, _) | (_, VirtualUtimeSpec::Omit) => Some(self.stat(path)?),
            _ => None,
        };
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let atime_ms = match atime {
            VirtualUtimeSpec::Set(spec) => spec.to_truncated_millis()?,
            VirtualUtimeSpec::Now => now_ms,
            VirtualUtimeSpec::Omit => {
                existing
                    .as_ref()
                    .ok_or_else(|| {
                        VfsError::new("EINVAL", "UTIME_OMIT requires existing metadata")
                    })?
                    .atime_ms
            }
        };
        let mtime_ms = match mtime {
            VirtualUtimeSpec::Set(spec) => spec.to_truncated_millis()?,
            VirtualUtimeSpec::Now => now_ms,
            VirtualUtimeSpec::Omit => {
                existing
                    .as_ref()
                    .ok_or_else(|| {
                        VfsError::new("EINVAL", "UTIME_OMIT requires existing metadata")
                    })?
                    .mtime_ms
            }
        };
        self.utimes(path, atime_ms, mtime_ms)
    }
    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()>;
    fn sync(&mut self, _path: &str) -> VfsResult<()> {
        Ok(())
    }
    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "allocation range overflows"))?;
        if length == 0 {
            return Ok(());
        }
        let stat = self.stat(path)?;
        if end > stat.size {
            self.truncate(path, end)?;
        }
        let mut cursor = offset;
        while cursor < end {
            let chunk_len = (end - cursor).min(64 * 1024) as usize;
            let mut bytes = self.pread(path, cursor, chunk_len)?;
            bytes.resize(chunk_len, 0);
            self.pwrite(path, bytes, cursor)?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()>;
    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()>;
    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "zero range overflows"))?;
        if length == 0 {
            return Err(VfsError::new("EINVAL", "zero range length must be nonzero"));
        }
        let original_size = self.stat(path)?.size;
        self.allocate(path, offset, length)?;
        let zero_end = if keep_size {
            end.min(original_size)
        } else {
            end
        };
        let mut cursor = offset.min(zero_end);
        while cursor < zero_end {
            let chunk_len = (zero_end - cursor).min(64 * 1024) as usize;
            self.pwrite(path, vec![0; chunk_len], cursor)?;
            cursor += chunk_len as u64;
        }
        if keep_size && self.stat(path)?.size != original_size {
            self.truncate(path, original_size)?;
        }
        Ok(())
    }
    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let requested_end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::new("EINVAL", "hole-punch range overflows"))?;
        let size = self.stat(path)?.size;
        let end = requested_end.min(size);
        let mut cursor = offset.min(size);
        while cursor < end {
            let chunk_len = (end - cursor).min(64 * 1024) as usize;
            self.pwrite(path, vec![0; chunk_len], cursor)?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Err(VfsError::new(
            "EOPNOTSUPP",
            format!("extent mapping is not supported for {path}"),
        ))
    }
    fn unwritten_ranges(&mut self, _path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Ok(Vec::new())
    }
    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        let allocated = self.allocated_ranges(path)?;
        let unwritten = self.unwritten_ranges(path)?;
        Ok(crate::extent::classified_file_extent_at(
            allocated.iter().copied(),
            unwritten.iter().copied(),
            index,
        )
        .map(|extent| FileExtent {
            start: extent.start,
            end: extent.end,
            unwritten: extent.unwritten,
        }))
    }
    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>>;
    fn pwrite(&mut self, path: &str, content: Vec<u8>, offset: u64) -> VfsResult<()> {
        let mut existing = self.read_file(path)?;
        let start = usize::try_from(offset).map_err(|_| {
            VfsError::new("EINVAL", format!("pwrite offset is too large: {offset}"))
        })?;
        if start > existing.len() {
            existing.resize(start, 0);
        }
        let end = start.checked_add(content.len()).ok_or_else(|| {
            VfsError::new(
                "EINVAL",
                format!(
                    "pwrite range overflows usize: offset={offset}, length={}",
                    content.len()
                ),
            )
        })?;
        if end > existing.len() {
            existing.resize(end, 0);
        }
        existing[start..end].copy_from_slice(&content);
        self.write_file(path, existing)
    }
    fn shutdown(&mut self) -> VfsResult<()> {
        Ok(())
    }
}

pub struct MountedVirtualFileSystem<F> {
    inner: F,
}

impl<F> MountedVirtualFileSystem<F> {
    pub fn new(inner: F) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &F {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut F {
        &mut self.inner
    }
}

impl<F> MountedFileSystem for MountedVirtualFileSystem<F>
where
    F: VirtualFileSystem + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        VirtualFileSystem::read_file(&mut self.inner, path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        VirtualFileSystem::read_dir(&mut self.inner, path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        VirtualFileSystem::read_dir_limited(&mut self.inner, path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        VirtualFileSystem::read_dir_with_types(&mut self.inner, path)
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        VirtualFileSystem::write_file(&mut self.inner, path, content)
    }

    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        VirtualFileSystem::write_file_with_mode(&mut self.inner, path, content, mode)
    }

    fn create_file_exclusive(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        VirtualFileSystem::create_file_exclusive(&mut self.inner, path, content)
    }

    fn create_file_exclusive_with_mode(
        &mut self,
        path: &str,
        content: Vec<u8>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        VirtualFileSystem::create_file_exclusive_with_mode(&mut self.inner, path, content, mode)
    }

    fn append_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<u64> {
        VirtualFileSystem::append_file(&mut self.inner, path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        VirtualFileSystem::create_dir(&mut self.inner, path)
    }

    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> VfsResult<()> {
        VirtualFileSystem::create_dir_with_mode(&mut self.inner, path, mode)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        VirtualFileSystem::mkdir(&mut self.inner, path, recursive)
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        VirtualFileSystem::mknod(&mut self.inner, path, mode, rdev)
    }

    fn mkdir_with_mode(&mut self, path: &str, recursive: bool, mode: Option<u32>) -> VfsResult<()> {
        VirtualFileSystem::mkdir_with_mode(&mut self.inner, path, recursive, mode)
    }

    fn exists(&self, path: &str) -> bool {
        VirtualFileSystem::exists(&self.inner, path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        VirtualFileSystem::stat(&mut self.inner, path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        VirtualFileSystem::remove_file(&mut self.inner, path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        VirtualFileSystem::remove_dir(&mut self.inner, path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        VirtualFileSystem::rename(&mut self.inner, old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        VirtualFileSystem::realpath(&self.inner, path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        VirtualFileSystem::symlink(&mut self.inner, target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        VirtualFileSystem::read_link(&self.inner, path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        VirtualFileSystem::lstat(&self.inner, path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        VirtualFileSystem::link(&mut self.inner, old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        VirtualFileSystem::chmod(&mut self.inner, path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        VirtualFileSystem::chown(&mut self.inner, path, uid, gid)
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        VirtualFileSystem::chown_spec(&mut self.inner, path, uid, gid, follow_symlinks)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        VirtualFileSystem::lchown(&mut self.inner, path, uid, gid)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        VirtualFileSystem::get_xattr(&mut self.inner, path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        VirtualFileSystem::list_xattrs(&mut self.inner, path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        VirtualFileSystem::set_xattr(&mut self.inner, path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        VirtualFileSystem::remove_xattr(&mut self.inner, path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        VirtualFileSystem::utimes(&mut self.inner, path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        VirtualFileSystem::utimes_spec(&mut self.inner, path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        VirtualFileSystem::truncate(&mut self.inner, path, length)
    }

    fn sync(&mut self, path: &str) -> VfsResult<()> {
        VirtualFileSystem::sync(&mut self.inner, path)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        VirtualFileSystem::allocate(&mut self.inner, path, offset, length)
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        VirtualFileSystem::insert_range(&mut self.inner, path, offset, length)
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        VirtualFileSystem::collapse_range(&mut self.inner, path, offset, length)
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        VirtualFileSystem::zero_range(&mut self.inner, path, offset, length, keep_size)
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        VirtualFileSystem::punch_hole(&mut self.inner, path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        VirtualFileSystem::allocated_ranges(&mut self.inner, path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        VirtualFileSystem::unwritten_ranges(&mut self.inner, path)
    }

    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        VirtualFileSystem::extent_at(&mut self.inner, path, index)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        VirtualFileSystem::pread(&mut self.inner, path, offset, length)
    }

    fn pwrite(&mut self, path: &str, content: Vec<u8>, offset: u64) -> VfsResult<()> {
        VirtualFileSystem::pwrite(&mut self.inner, path, content, offset)
    }
}

impl<T> MountedFileSystem for Box<T>
where
    T: MountedFileSystem + ?Sized + 'static,
{
    fn as_any(&self) -> &dyn Any {
        (**self).as_any()
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        (**self).as_any_mut()
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        (**self).read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        (**self).read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        (**self).read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        (**self).read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        (**self).write_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        (**self).create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        (**self).mkdir(path, recursive)
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        (**self).mknod(path, mode, rdev)
    }

    fn exists(&self, path: &str) -> bool {
        (**self).exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        (**self).stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        (**self).remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        (**self).remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        (**self).rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        (**self).realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        (**self).symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        (**self).read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        (**self).lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        (**self).link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        (**self).chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        (**self).chown(path, uid, gid)
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        (**self).chown_spec(path, uid, gid, follow_symlinks)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        (**self).lchown(path, uid, gid)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        (**self).get_xattr(path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        (**self).list_xattrs(path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        (**self).set_xattr(path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        (**self).remove_xattr(path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        (**self).utimes(path, atime_ms, mtime_ms)
    }

    fn set_atime(&mut self, path: &str, atime_ms: u64) -> VfsResult<()> {
        (**self).set_atime(path, atime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        (**self).utimes_spec(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        (**self).truncate(path, length)
    }

    fn sync(&mut self, path: &str) -> VfsResult<()> {
        (**self).sync(path)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        (**self).allocate(path, offset, length)
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        (**self).insert_range(path, offset, length)
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        (**self).collapse_range(path, offset, length)
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        (**self).zero_range(path, offset, length, keep_size)
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        (**self).punch_hole(path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        (**self).allocated_ranges(path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        (**self).unwritten_ranges(path)
    }

    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        (**self).extent_at(path, index)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        (**self).pread(path, offset, length)
    }

    fn pwrite(&mut self, path: &str, content: Vec<u8>, offset: u64) -> VfsResult<()> {
        (**self).pwrite(path, content, offset)
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        (**self).shutdown()
    }
}

pub struct ReadOnlyFileSystem<F> {
    inner: F,
}

impl<F> ReadOnlyFileSystem<F> {
    pub fn new(inner: F) -> Self {
        Self { inner }
    }
}

impl<F> MountedFileSystem for ReadOnlyFileSystem<F>
where
    F: MountedFileSystem + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        self.inner.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        self.inner.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, _content: Vec<u8>) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn mkdir(&mut self, path: &str, _recursive: bool) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn mknod(&mut self, path: &str, _mode: u32, _rdev: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn rename(&mut self, old_path: &str, _new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {old_path}"),
        ))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        self.inner.realpath(path)
    }

    fn symlink(&mut self, _target: &str, link_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {link_path}"),
        ))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        self.inner.lstat(path)
    }

    fn link(&mut self, _old_path: &str, new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {new_path}"),
        ))
    }

    fn chmod(&mut self, path: &str, _mode: u32) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn chown(&mut self, path: &str, _uid: u32, _gid: u32) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn chown_spec(
        &mut self,
        path: &str,
        _uid: u32,
        _gid: u32,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        self.inner.get_xattr(path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        self.inner.list_xattrs(path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        _name: &str,
        _value: Vec<u8>,
        _flags: u32,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn remove_xattr(&mut self, path: &str, _name: &str, _follow_symlinks: bool) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn utimes(&mut self, path: &str, _atime_ms: u64, _mtime_ms: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn set_atime(&mut self, _path: &str, _atime_ms: u64) -> VfsResult<()> {
        Ok(())
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        _atime: VirtualUtimeSpec,
        _mtime: VirtualUtimeSpec,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn truncate(&mut self, path: &str, _length: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn sync(&mut self, path: &str) -> VfsResult<()> {
        self.inner.sync(path)
    }

    fn allocate(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn insert_range(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn collapse_range(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn zero_range(
        &mut self,
        path: &str,
        _offset: u64,
        _length: u64,
        _keep_size: bool,
    ) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn punch_hole(&mut self, path: &str, _offset: u64, _length: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        self.inner.allocated_ranges(path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        self.inner.unwritten_ranges(path)
    }

    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        self.inner.extent_at(path, index)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }

    fn pwrite(&mut self, path: &str, _content: Vec<u8>, _offset: u64) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only filesystem: {path}"),
        ))
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        self.inner.shutdown()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessTimePolicy {
    Relatime,
    NoAtime,
    StrictAtime,
}

impl AccessTimePolicy {
    pub fn option_name(&self) -> &'static str {
        match self {
            Self::Relatime => "relatime",
            Self::NoAtime => "noatime",
            Self::StrictAtime => "strictatime",
        }
    }
}

impl MountEntry {
    pub fn option_string(&self) -> String {
        let mut options = vec![
            if self.read_only { "ro" } else { "rw" },
            self.access_time.option_name(),
        ];
        if self.no_dir_atime {
            options.push("nodiratime");
        }
        options.join(",")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub path: String,
    pub plugin_id: String,
    pub guest_source: String,
    pub guest_fstype: String,
    pub read_only: bool,
    pub access_time: AccessTimePolicy,
    pub no_dir_atime: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOptions {
    pub plugin_id: String,
    pub guest_source: String,
    pub guest_fstype: String,
    pub read_only: bool,
    pub access_time: AccessTimePolicy,
    pub no_dir_atime: bool,
    pub max_bytes: Option<u64>,
    pub max_inodes: Option<usize>,
}

impl MountOptions {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        let plugin_id = plugin_id.into();
        Self {
            guest_source: plugin_id.clone(),
            guest_fstype: plugin_id.clone(),
            plugin_id,
            read_only: false,
            access_time: AccessTimePolicy::Relatime,
            no_dir_atime: false,
            max_bytes: None,
            max_inodes: None,
        }
    }

    pub fn guest_source(mut self, guest_source: impl Into<String>) -> Self {
        self.guest_source = guest_source.into();
        self
    }

    pub fn guest_fstype(mut self, guest_fstype: impl Into<String>) -> Self {
        self.guest_fstype = guest_fstype.into();
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn access_time(mut self, access_time: AccessTimePolicy) -> Self {
        self.access_time = access_time;
        self
    }

    pub fn no_dir_atime(mut self, no_dir_atime: bool) -> Self {
        self.no_dir_atime = no_dir_atime;
        self
    }

    pub fn max_bytes(mut self, max_bytes: Option<u64>) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn max_inodes(mut self, max_inodes: Option<usize>) -> Self {
        self.max_inodes = max_inodes;
        self
    }
}

struct MountRegistration {
    path: String,
    plugin_id: String,
    guest_source: String,
    guest_fstype: String,
    read_only: bool,
    access_time: AccessTimePolicy,
    no_dir_atime: bool,
    max_bytes: Option<u64>,
    max_inodes: Option<usize>,
    cached_usage: Option<FileSystemUsage>,
    filesystem: Box<dyn MountedFileSystem>,
}

pub struct MountTable {
    mounts: Vec<MountRegistration>,
    mount_indices: BTreeMap<String, usize>,
}

impl MountTable {
    pub fn new(root_fs: impl VirtualFileSystem + 'static) -> Self {
        Self {
            mounts: vec![MountRegistration {
                path: String::from("/"),
                plugin_id: String::from("root"),
                guest_source: String::from("root"),
                guest_fstype: String::from("root"),
                read_only: false,
                access_time: AccessTimePolicy::Relatime,
                no_dir_atime: false,
                max_bytes: None,
                max_inodes: None,
                cached_usage: None,
                filesystem: Box::new(MountedVirtualFileSystem::new(root_fs)),
            }],
            mount_indices: BTreeMap::from([(String::from("/"), 0)]),
        }
    }

    pub fn new_boxed_root(filesystem: Box<dyn MountedFileSystem>, options: MountOptions) -> Self {
        let filesystem = if options.read_only {
            Box::new(ReadOnlyFileSystem::new(filesystem)) as Box<dyn MountedFileSystem>
        } else {
            filesystem
        };

        Self {
            mounts: vec![MountRegistration {
                path: String::from("/"),
                plugin_id: options.plugin_id,
                guest_source: options.guest_source,
                guest_fstype: options.guest_fstype,
                read_only: options.read_only,
                access_time: options.access_time,
                no_dir_atime: options.no_dir_atime,
                max_bytes: options.max_bytes,
                max_inodes: options.max_inodes,
                cached_usage: None,
                filesystem,
            }],
            mount_indices: BTreeMap::from([(String::from("/"), 0)]),
        }
    }

    pub fn mount(
        &mut self,
        path: &str,
        filesystem: impl VirtualFileSystem + 'static,
        options: MountOptions,
    ) -> VfsResult<()> {
        self.mount_boxed(
            path,
            Box::new(MountedVirtualFileSystem::new(filesystem)),
            options,
        )
    }

    pub fn mount_boxed(
        &mut self,
        path: &str,
        mut filesystem: Box<dyn MountedFileSystem>,
        options: MountOptions,
    ) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Err(VfsError::new("EINVAL", "cannot mount over root"));
        }
        if self.mounts.iter().any(|mount| mount.path == normalized) {
            return Err(VfsError::new(
                "EEXIST",
                format!("already mounted at {normalized}"),
            ));
        }

        let (parent_index, relative_path) = self.resolve_index(&normalized)?;
        let parent_mount = &mut self.mounts[parent_index];
        if !parent_mount.filesystem.exists(&relative_path) {
            // Materializing the mountpoint directory on the parent is
            // cosmetic: child mounts resolve by path prefix before the parent
            // is consulted. A read-only parent (for example a read-only
            // module-access mount hosting nested package mounts) cannot
            // materialize the entry, but the mount must still succeed.
            if let Err(error) = parent_mount.filesystem.mkdir(&relative_path, true) {
                if error.code() != "EROFS" {
                    if let Err(shutdown_error) = filesystem.shutdown() {
                        return Err(VfsError::new(
                            shutdown_error.code(),
                            format!(
                                "failed to shut down filesystem after mount failure ({error}): {}",
                                shutdown_error.message()
                            ),
                        ));
                    }

                    return Err(error);
                }
            }
        }

        let filesystem = if options.read_only {
            Box::new(ReadOnlyFileSystem::new(filesystem)) as Box<dyn MountedFileSystem>
        } else {
            filesystem
        };

        self.mounts.push(MountRegistration {
            path: normalized,
            plugin_id: options.plugin_id,
            guest_source: options.guest_source,
            guest_fstype: options.guest_fstype,
            read_only: options.read_only,
            access_time: options.access_time,
            no_dir_atime: options.no_dir_atime,
            max_bytes: options.max_bytes,
            max_inodes: options.max_inodes,
            cached_usage: None,
            filesystem,
        });
        self.mounts
            .sort_by_key(|mount| std::cmp::Reverse(mount.path.len()));
        self.rebuild_mount_indices();
        Ok(())
    }

    pub fn unmount(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Err(VfsError::new("EINVAL", "cannot unmount root"));
        }

        let child_mount_prefix = format!("{normalized}/");
        if self
            .mounts
            .iter()
            .any(|mount| mount.path.starts_with(&child_mount_prefix))
        {
            return Err(VfsError::new(
                "EBUSY",
                format!("mount point has child mounts: {normalized}"),
            ));
        }

        let Some(index) = self
            .mounts
            .iter()
            .position(|mount| mount.path == normalized)
        else {
            return Err(VfsError::new(
                "EINVAL",
                format!("not a mount point: {normalized}"),
            ));
        };

        let mut mount = self.mounts.remove(index);
        self.rebuild_mount_indices();
        mount.filesystem.shutdown()?;
        Ok(())
    }

    pub fn remount(&mut self, path: &str, options: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        let mount = self
            .mounts
            .iter_mut()
            .find(|mount| mount.path == normalized)
            .ok_or_else(|| VfsError::new("EINVAL", format!("not a mount point: {normalized}")))?;

        let mut read_only = mount.read_only;
        let mut access_time = mount.access_time.clone();
        let mut no_dir_atime = mount.no_dir_atime;
        let mut max_bytes = mount.max_bytes;
        let mut max_inodes = mount.max_inodes;
        for option in options
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            match option {
                "remount" => {}
                "ro" => read_only = true,
                "rw" => read_only = false,
                "relatime" => access_time = AccessTimePolicy::Relatime,
                "noatime" => access_time = AccessTimePolicy::NoAtime,
                "strictatime" => access_time = AccessTimePolicy::StrictAtime,
                "nodiratime" => no_dir_atime = true,
                "diratime" => no_dir_atime = false,
                value if value.starts_with("size=") => {
                    max_bytes = Some(parse_mount_limit(value, "size")?);
                }
                value if value.starts_with("inodes=") => {
                    max_inodes = Some(
                        usize::try_from(parse_mount_limit(value, "inodes")?).map_err(|_| {
                            VfsError::new("EINVAL", "mount inode limit exceeds usize")
                        })?,
                    );
                }
                unsupported => {
                    return Err(VfsError::new(
                        "EINVAL",
                        format!("unsupported mount option: {unsupported}"),
                    ));
                }
            }
        }
        let usage =
            measure_mounted_filesystem_usage(mount.filesystem.as_mut(), "/", &mut BTreeSet::new())?;
        check_usage_limits(&usage, max_bytes, max_inodes)?;
        mount.read_only = read_only;
        mount.access_time = access_time;
        mount.no_dir_atime = no_dir_atime;
        mount.max_bytes = max_bytes;
        mount.max_inodes = max_inodes;
        mount.cached_usage = Some(usage);
        Ok(())
    }

    pub fn get_mounts(&self) -> Vec<MountEntry> {
        self.mounts
            .iter()
            .map(|mount| MountEntry {
                path: mount.path.clone(),
                plugin_id: mount.plugin_id.clone(),
                guest_source: mount.guest_source.clone(),
                guest_fstype: mount.guest_fstype.clone(),
                read_only: mount.read_only,
                access_time: mount.access_time.clone(),
                no_dir_atime: mount.no_dir_atime,
            })
            .collect()
    }

    pub fn root_virtual_filesystem_mut<T: VirtualFileSystem + 'static>(
        &mut self,
    ) -> Option<&mut T> {
        let root = self.mounts.iter_mut().find(|mount| mount.path == "/")?;
        root.filesystem
            .as_any_mut()
            .downcast_mut::<MountedVirtualFileSystem<T>>()
            .map(MountedVirtualFileSystem::inner_mut)
    }

    pub fn check_rename_copy_up_limits(
        &mut self,
        old_path: &str,
        new_path: &str,
        max_bytes: Option<u64>,
        max_inodes: Option<usize>,
    ) -> VfsResult<()> {
        let (old_index, old_relative_path) = self.resolve_index(old_path)?;
        let (new_index, new_relative_path) = self.resolve_index(new_path)?;
        if old_index != new_index {
            return Ok(());
        }

        let filesystem = &mut self.mounts[old_index].filesystem;
        if let Some(root) = filesystem
            .as_any_mut()
            .downcast_mut::<MountedVirtualFileSystem<RootFileSystem>>()
        {
            root.inner_mut().check_rename_copy_up_limits(
                &old_relative_path,
                &new_relative_path,
                max_bytes,
                max_inodes,
            )?;
        }

        Ok(())
    }

    pub fn root_usage(&mut self) -> VfsResult<FileSystemUsage> {
        let root = self
            .mounts
            .iter_mut()
            .find(|mount| mount.path == "/")
            .ok_or_else(|| VfsError::new("ENOENT", "missing root mount"))?;
        measure_mounted_filesystem_usage(root.filesystem.as_mut(), "/", &mut BTreeSet::new())
    }

    pub fn path_stats(
        &mut self,
        path: &str,
        root_max_bytes: Option<u64>,
        root_max_inodes: Option<usize>,
    ) -> VfsResult<FileSystemStats> {
        let (index, _) = self.resolve_content_index(path)?;
        let mount = &mut self.mounts[index];
        let usage =
            measure_mounted_filesystem_usage(mount.filesystem.as_mut(), "/", &mut BTreeSet::new())?;
        mount.cached_usage = Some(usage.clone());
        let max_bytes = mount
            .max_bytes
            .or_else(|| (mount.path == "/").then_some(root_max_bytes).flatten())
            .unwrap_or(usage.total_bytes);
        let used_bytes = usage.total_bytes.min(max_bytes);
        let max_inodes = mount
            .max_inodes
            .or_else(|| (mount.path == "/").then_some(root_max_inodes).flatten())
            .map(|value| value as u64)
            .unwrap_or(usage.inode_count as u64);
        let used_inodes = (usage.inode_count as u64).min(max_inodes);
        Ok(FileSystemStats {
            total_bytes: max_bytes,
            used_bytes,
            available_bytes: max_bytes.saturating_sub(used_bytes),
            total_inodes: max_inodes,
            free_inodes: max_inodes.saturating_sub(used_inodes),
        })
    }

    fn cached_usage(&mut self, index: usize) -> VfsResult<FileSystemUsage> {
        if let Some(usage) = self.mounts[index].cached_usage.clone() {
            return Ok(usage);
        }
        let usage = measure_mounted_filesystem_usage(
            self.mounts[index].filesystem.as_mut(),
            "/",
            &mut BTreeSet::new(),
        )?;
        self.mounts[index].cached_usage = Some(usage.clone());
        Ok(usage)
    }

    fn update_cached_path_usage(
        &mut self,
        index: usize,
        before: Option<VirtualStat>,
        relative_path: &str,
    ) {
        let after = self.mounts[index].filesystem.lstat(relative_path).ok();
        let Some(usage) = self.mounts[index].cached_usage.as_mut() else {
            return;
        };
        match (before, after) {
            (None, Some(after)) => {
                usage.inode_count = usage.inode_count.saturating_add(1);
                if !after.is_directory {
                    usage.total_bytes = usage.total_bytes.saturating_add(after.size);
                }
            }
            (Some(before), None) => {
                if before.is_directory || before.nlink <= 1 {
                    usage.inode_count = usage.inode_count.saturating_sub(1);
                    if !before.is_directory {
                        usage.total_bytes = usage.total_bytes.saturating_sub(before.size);
                    }
                }
            }
            (Some(before), Some(after))
                if (before.dev, before.ino) == (after.dev, after.ino) && !before.is_directory =>
            {
                usage.total_bytes = usage
                    .total_bytes
                    .saturating_sub(before.size)
                    .saturating_add(after.size);
            }
            _ => {}
        }
    }

    fn check_file_growth(
        &mut self,
        index: usize,
        relative_path: &str,
        new_size: u64,
        exclusive: bool,
    ) -> VfsResult<()> {
        if self.mounts[index].max_bytes.is_none() && self.mounts[index].max_inodes.is_none() {
            return Ok(());
        }
        let usage = self.cached_usage(index)?;
        let mount = &mut self.mounts[index];
        let existing = mount.filesystem.lstat(relative_path).ok();
        let existing_size = existing
            .as_ref()
            .filter(|stat| !stat.is_directory)
            .map_or(0, |stat| stat.size);
        let resulting = FileSystemUsage {
            total_bytes: usage
                .total_bytes
                .saturating_sub(existing_size)
                .saturating_add(new_size),
            inode_count: usage
                .inode_count
                .saturating_add(usize::from(existing.is_none())),
        };
        if exclusive && existing.is_some() {
            return Ok(());
        }
        check_usage_limits(&resulting, mount.max_bytes, mount.max_inodes)
    }

    fn check_inode_growth(&mut self, index: usize, added: usize) -> VfsResult<()> {
        if added == 0
            || (self.mounts[index].max_bytes.is_none() && self.mounts[index].max_inodes.is_none())
        {
            return Ok(());
        }
        let mut usage = self.cached_usage(index)?;
        let mount = &self.mounts[index];
        usage.inode_count = usage.inode_count.saturating_add(added);
        check_usage_limits(&usage, mount.max_bytes, mount.max_inodes)
    }

    fn missing_directory_count(&self, index: usize, relative_path: &str) -> usize {
        let mut current = String::from("/");
        let mut missing = 0usize;
        for component in path_components(relative_path) {
            current = join_path(&current, &component);
            if !self.mounts[index].filesystem.exists(&current) {
                missing = missing.saturating_add(1);
            }
        }
        missing
    }

    pub fn path_uses_root_filesystem(&self, path: &str) -> bool {
        self.resolve_index(path)
            .is_ok_and(|(index, _)| self.mounts[index].path == "/")
    }

    fn resolve_index(&self, full_path: &str) -> VfsResult<(usize, String)> {
        let normalized = normalize_path(full_path);
        let mut candidate = normalized.as_str();
        loop {
            if let Some(index) = self.mount_indices.get(candidate).copied() {
                let relative_path = if candidate == "/" {
                    normalized
                } else if candidate.len() == normalized.len() {
                    String::from("/")
                } else {
                    // Strip exactly the mount prefix once. `trim_start_matches`
                    // would strip repeated path components and alias distinct
                    // paths such as `/data/data/file` and `/data/file`.
                    format!("/{}", &normalized[candidate.len() + 1..])
                };
                return Ok((index, relative_path));
            }
            if candidate == "/" {
                break;
            }
            candidate = candidate
                .rfind('/')
                .map(|index| if index == 0 { "/" } else { &candidate[..index] })
                .unwrap_or("/");
        }

        Err(VfsError::new(
            "ENOENT",
            format!("no such file or directory, resolve '{full_path}'"),
        ))
    }

    fn rebuild_mount_indices(&mut self) {
        self.mount_indices.clear();
        self.mount_indices.extend(
            self.mounts
                .iter()
                .enumerate()
                .map(|(index, mount)| (mount.path.clone(), index)),
        );
    }

    fn resolve_writable_index(&self, full_path: &str) -> VfsResult<(usize, String)> {
        let (index, relative_path) = self.resolve_index(full_path)?;
        self.ensure_writable(index, full_path)?;
        Ok((index, relative_path))
    }

    fn ensure_writable(&self, index: usize, full_path: &str) -> VfsResult<()> {
        if self.mounts[index].read_only {
            return Err(VfsError::new(
                "EROFS",
                format!("read-only filesystem: {full_path}"),
            ));
        }
        Ok(())
    }

    fn atime_snapshot(
        &mut self,
        index: usize,
        relative_path: &str,
        is_directory: bool,
    ) -> VfsResult<Option<VirtualStat>> {
        let mount = &mut self.mounts[index];
        if mount.read_only
            || mount.access_time == AccessTimePolicy::NoAtime
            || (is_directory && mount.no_dir_atime)
        {
            return Ok(None);
        }
        mount.filesystem.stat(relative_path).map(Some)
    }

    fn finish_atime_update(
        &mut self,
        index: usize,
        relative_path: &str,
        before: Option<VirtualStat>,
    ) -> VfsResult<()> {
        let Some(before) = before else {
            return Ok(());
        };
        let mount = &mut self.mounts[index];
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let update = match mount.access_time {
            AccessTimePolicy::NoAtime => false,
            AccessTimePolicy::StrictAtime => true,
            AccessTimePolicy::Relatime => {
                timestamp_ns(before.atime_ms, before.atime_nsec)
                    <= timestamp_ns(before.mtime_ms, before.mtime_nsec)
                    || timestamp_ns(before.atime_ms, before.atime_nsec)
                        <= timestamp_ns(before.ctime_ms, before.ctime_nsec)
                    || now_ms.saturating_sub(before.atime_ms) >= 24 * 60 * 60 * 1_000
            }
        };
        if update {
            mount.filesystem.set_atime(relative_path, now_ms)?;
        }
        Ok(())
    }

    /// Resolve a path for a CONTENT operation (read_file/stat/pread/read_dir)
    /// that must follow symlinks like POSIX `open()`. `resolve_index` is purely
    /// lexical, so a path that descends through a symlink whose target lives in a
    /// *different* mount (e.g. `/opt/agentos/pkgs/<pkg>/current -> <version>`,
    /// where `current` is its own single-symlink leaf mount) would route into the
    /// symlink mount and fail. `realpath` follows those cross-mount symlinks, so
    /// resolve it first, then route the resolved path. Falls back to the raw path
    /// when realpath can't resolve it (e.g. a genuinely missing file) so callers
    /// still receive the mount's own ENOENT.
    /// Resolve a path for a LINK-LEAF operation (lstat/readlink) that must
    /// follow INTERMEDIATE symlinks but not the final component. When the
    /// lexical route has a symlinked parent, resolve that parent across mounts
    /// and re-route while keeping the final component unresolved. `relative ==
    /// "/"` (a mounted symlink leaf itself) keeps the raw route so
    /// `lstat(<pkg>/current)` still reports the symlink.
    fn resolve_link_leaf_index(&self, path: &str) -> VfsResult<(usize, String)> {
        let normalized = normalize_path(path);
        let raw = self.resolve_index(&normalized)?;
        if raw.1 == "/" {
            return Ok(raw);
        }
        let parent = parent_path(&normalized);
        let leaf = basename(&normalized);
        match self.realpath(&parent) {
            Ok(resolved_parent) if resolved_parent != parent => {
                self.resolve_index(&join_path(&resolved_parent, &leaf))
            }
            Err(_) => Ok(raw),
            Ok(_) => Ok(raw),
        }
    }

    fn resolve_content_index(&self, path: &str) -> VfsResult<(usize, String)> {
        let normalized = normalize_path(path);
        let raw = self.resolve_index(&normalized)?;
        match self.realpath(&normalized) {
            Ok(resolved) if resolved != normalized => self.resolve_index(&resolved),
            Ok(_) | Err(_) => Ok(raw),
        }
    }

    fn child_mount_basenames(&self, path: &str) -> Vec<String> {
        let normalized = normalize_path(path);
        let mut basenames = BTreeSet::new();
        for mount in &self.mounts {
            if mount.path == "/" || mount.path == normalized {
                continue;
            }

            if parent_path(&mount.path) == normalized {
                basenames.insert(basename(&mount.path));
            }
        }
        basenames.into_iter().collect()
    }

    fn realpath_in_mount(&self, index: usize, relative_path: &str) -> VfsResult<String> {
        let mount = &self.mounts[index];
        let resolved = mount.filesystem.realpath(relative_path)?;
        if mount.path == "/" {
            return Ok(normalize_path(&resolved));
        }
        if resolved == "/" {
            return Ok(mount.path.clone());
        }
        Ok(normalize_path(&format!(
            "{}/{}",
            mount.path,
            resolved.trim_start_matches('/')
        )))
    }
}

fn measure_mounted_filesystem_usage(
    filesystem: &mut dyn MountedFileSystem,
    path: &str,
    visited: &mut BTreeSet<(u64, u64)>,
) -> VfsResult<FileSystemUsage> {
    let stat = filesystem.lstat(path)?;
    let mut usage = FileSystemUsage::default();

    if visited.insert((stat.dev, stat.ino)) {
        usage.inode_count += 1;
        if !stat.is_directory {
            usage.total_bytes = usage.total_bytes.saturating_add(stat.size);
        }
    }

    if !stat.is_directory || stat.is_symbolic_link {
        return Ok(usage);
    }

    for entry in filesystem.read_dir_with_types(path)? {
        if matches!(entry.name.as_str(), "." | "..") {
            continue;
        }

        let child_path = if path == "/" {
            format!("/{}", entry.name)
        } else {
            format!("{path}/{}", entry.name)
        };
        let child_usage = measure_mounted_filesystem_usage(filesystem, &child_path, visited)?;
        usage.total_bytes = usage.total_bytes.saturating_add(child_usage.total_bytes);
        usage.inode_count = usage.inode_count.saturating_add(child_usage.inode_count);
    }

    Ok(usage)
}

impl Drop for MountTable {
    fn drop(&mut self) {
        for mount in self.mounts.iter_mut().rev() {
            if let Err(error) = mount.filesystem.shutdown() {
                eprintln!(
                    "failed to shut down filesystem mounted at {}: {}: {}",
                    mount.path,
                    error.code(),
                    error.message()
                );
            }
        }
    }
}

impl VirtualFileSystem for MountTable {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        let before = self.atime_snapshot(index, &relative_path, false)?;
        let content = self.mounts[index].filesystem.read_file(&relative_path)?;
        self.finish_atime_update(index, &relative_path, before)?;
        Ok(content)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let normalized = normalize_path(path);
        // Directory listings are content ops: a path that descends through a
        // symlink-root leaf mount (`<pkg>/current -> <version>`) must be
        // followed into the target mount, exactly like read_file/stat. Child
        // mounts still merge on the LEXICAL path — mount points attach to the
        // caller-visible path, not the resolved target.
        let (index, relative_path) = self.resolve_content_index(&normalized)?;
        let before = self.atime_snapshot(index, &relative_path, true)?;
        let mut entries = self.mounts[index].filesystem.read_dir(&relative_path)?;
        self.finish_atime_update(index, &relative_path, before)?;
        let child_mounts = self.child_mount_basenames(&normalized);
        if child_mounts.is_empty() {
            return Ok(entries);
        }

        let mut merged = BTreeSet::new();
        merged.extend(entries.drain(..));
        merged.extend(child_mounts);
        Ok(merged.into_iter().collect())
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        let normalized = normalize_path(path);
        let (index, relative_path) = self.resolve_content_index(&normalized)?;
        let before = self.atime_snapshot(index, &relative_path, true)?;
        let mut entries = self.mounts[index]
            .filesystem
            .read_dir_limited(&relative_path, max_entries)?;
        self.finish_atime_update(index, &relative_path, before)?;
        let child_mounts = self.child_mount_basenames(&normalized);
        if child_mounts.is_empty() {
            return Ok(entries);
        }

        let mut merged = BTreeSet::new();
        merged.extend(entries.drain(..));
        merged.extend(child_mounts);
        if merged.len() > max_entries {
            return Err(VfsError::new(
                "ENOMEM",
                format!(
                    "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                ),
            ));
        }
        Ok(merged.into_iter().collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let normalized = normalize_path(path);
        let (index, relative_path) = self.resolve_content_index(&normalized)?;
        let before = self.atime_snapshot(index, &relative_path, true)?;
        let mut entries = self.mounts[index]
            .filesystem
            .read_dir_with_types(&relative_path)?;
        self.finish_atime_update(index, &relative_path, before)?;
        let child_mounts = self.child_mount_basenames(&normalized);
        if child_mounts.is_empty() {
            return Ok(entries);
        }

        let existing = entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<BTreeSet<_>>();
        for mount_name in child_mounts {
            if existing.contains(&mount_name) {
                continue;
            }
            entries.push(VirtualDirEntry {
                name: mount_name,
                is_directory: true,
                is_symbolic_link: false,
            });
        }
        Ok(entries)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = content.into();
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, content.len() as u64, false)?;
        self.mounts[index]
            .filesystem
            .write_file(&relative_path, content)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn write_file_with_mode(
        &mut self,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let content = content.into();
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, content.len() as u64, false)?;
        self.mounts[index]
            .filesystem
            .write_file_with_mode(&relative_path, content, mode)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        let content = content.into();
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, content.len() as u64, true)?;
        self.mounts[index]
            .filesystem
            .create_file_exclusive(&relative_path, content)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn create_file_exclusive_with_mode(
        &mut self,
        path: &str,
        content: impl Into<Vec<u8>>,
        mode: Option<u32>,
    ) -> VfsResult<()> {
        let content = content.into();
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, content.len() as u64, true)?;
        self.mounts[index]
            .filesystem
            .create_file_exclusive_with_mode(&relative_path, content, mode)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        let content = content.into();
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        let current_size = before.as_ref().map_or(0, |stat| stat.size);
        self.check_file_growth(
            index,
            &relative_path,
            current_size.saturating_add(content.len() as u64),
            false,
        )?;
        let size = self.mounts[index]
            .filesystem
            .append_file(&relative_path, content)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(size)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_inode_growth(
            index,
            usize::from(!self.mounts[index].filesystem.exists(&relative_path)),
        )?;
        self.mounts[index].filesystem.create_dir(&relative_path)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn create_dir_with_mode(&mut self, path: &str, mode: Option<u32>) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_inode_growth(
            index,
            usize::from(!self.mounts[index].filesystem.exists(&relative_path)),
        )?;
        self.mounts[index]
            .filesystem
            .create_dir_with_mode(&relative_path, mode)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        let added = if recursive {
            self.missing_directory_count(index, &relative_path)
        } else {
            usize::from(before.is_none())
        };
        self.check_inode_growth(index, added)?;
        self.mounts[index]
            .filesystem
            .mkdir(&relative_path, recursive)?;
        if recursive {
            if let Some(usage) = self.mounts[index].cached_usage.as_mut() {
                usage.inode_count = usage.inode_count.saturating_add(added);
            }
        } else {
            self.update_cached_path_usage(index, before, &relative_path);
        }
        Ok(())
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_inode_growth(
            index,
            usize::from(!self.mounts[index].filesystem.exists(&relative_path)),
        )?;
        self.mounts[index]
            .filesystem
            .mknod(&relative_path, mode, rdev)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn mkdir_with_mode(&mut self, path: &str, recursive: bool, mode: Option<u32>) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        let added = if recursive {
            self.missing_directory_count(index, &relative_path)
        } else {
            usize::from(before.is_none())
        };
        self.check_inode_growth(index, added)?;
        self.mounts[index]
            .filesystem
            .mkdir_with_mode(&relative_path, recursive, mode)?;
        if recursive {
            if let Some(usage) = self.mounts[index].cached_usage.as_mut() {
                usage.inode_count = usage.inode_count.saturating_add(added);
            }
        } else {
            self.update_cached_path_usage(index, before, &relative_path);
        }
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        // `exists` follows symlinks like POSIX access(); route through the
        // content resolver so paths under a symlink-root leaf mount resolve.
        self.resolve_content_index(path)
            .map(|(index, relative_path)| self.mounts[index].filesystem.exists(&relative_path))
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.mounts[index].filesystem.stat(&relative_path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.mounts[index].filesystem.remove_file(&relative_path)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.mounts[index].filesystem.remove_dir(&relative_path)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let (old_index, old_relative_path) = self.resolve_index(old_path)?;
        let (new_index, new_relative_path) = self.resolve_index(new_path)?;
        if old_index != new_index {
            return Err(VfsError::new(
                "EXDEV",
                format!("rename across mounts: {old_path} -> {new_path}"),
            ));
        }
        self.ensure_writable(old_index, old_path)?;
        self.ensure_writable(new_index, new_path)?;
        let source = self.mounts[old_index]
            .filesystem
            .lstat(&old_relative_path)
            .ok();
        let replaced = self.mounts[new_index]
            .filesystem
            .lstat(&new_relative_path)
            .ok();
        self.mounts[old_index]
            .filesystem
            .rename(&old_relative_path, &new_relative_path)?;
        if let (Some(source), Some(replaced), Some(usage)) = (
            source,
            replaced,
            self.mounts[old_index].cached_usage.as_mut(),
        ) {
            if (source.dev, source.ino) != (replaced.dev, replaced.ino)
                && (replaced.is_directory || replaced.nlink <= 1)
            {
                usage.inode_count = usage.inode_count.saturating_sub(1);
                if !replaced.is_directory {
                    usage.total_bytes = usage.total_bytes.saturating_sub(replaced.size);
                }
            }
        }
        Ok(())
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let normalized = normalize_path(path);
        let (index, relative_path) = self.resolve_index(&normalized)?;
        let fallback_error = match self.realpath_in_mount(index, &relative_path) {
            Ok(resolved) => return Ok(resolved),
            // ELOOP and ENOENT may mean the mounted backend encountered a symlink
            // target expressed in the guest namespace. Walk components through the
            // mount table, but preserve the backend's original ENOENT when the walk
            // does not actually encounter a symlink.
            Err(error) if error.code() == "ELOOP" => None,
            Err(error) if error.code() == "ENOENT" => Some(error),
            Err(error) => return Err(error),
        };

        let mut pending = path_components(&normalized);
        let mut current = String::from("/");
        let mut followed_symlinks = 0usize;

        while let Some(component) = pending.pop_front() {
            let candidate = join_path(&current, &component);
            let stat = match self.lstat(&candidate) {
                Ok(stat) => stat,
                Err(error) => return Err(fallback_error.unwrap_or(error)),
            };

            if stat.is_symbolic_link {
                followed_symlinks += 1;
                if followed_symlinks > MAX_REALPATH_SYMLINKS {
                    return Err(VfsError::new(
                        "ELOOP",
                        format!("too many levels of symbolic links, '{path}'"),
                    ));
                }

                // Mounted filesystems express absolute symlink targets in their
                // own root namespace. Keep the mount index while reading the
                // link so the component-walk fallback does not accidentally
                // reinterpret `/target` as the VM root after crossing a
                // synthetic leaf mount.
                let (link_index, relative_path) = self.resolve_link_leaf_index(&candidate)?;
                let target = self.mounts[link_index]
                    .filesystem
                    .read_link(&relative_path)?;
                let target_path = if target.starts_with('/') {
                    let mount_path = &self.mounts[link_index].path;
                    let guest_absolute_target =
                        target == *mount_path || target.starts_with(&format!("{mount_path}/"));
                    if mount_path == "/" || guest_absolute_target {
                        normalize_path(&target)
                    } else {
                        normalize_path(&format!(
                            "{}/{}",
                            mount_path,
                            target.trim_start_matches('/')
                        ))
                    }
                } else {
                    normalize_path(&format!("{}/{}", parent_path(&candidate), target))
                };
                let mut resolved_target = path_components(&target_path);
                resolved_target.extend(pending);
                pending = resolved_target;
                current = String::from("/");
                continue;
            }

            if !pending.is_empty() && !stat.is_directory {
                return Err(VfsError::new(
                    "ENOTDIR",
                    format!("not a directory, realpath '{candidate}'"),
                ));
            }

            current = candidate;
        }

        if followed_symlinks == 0 {
            if let Some(error) = fallback_error {
                return Err(error);
            }
        }
        Ok(current)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        let normalized_link_path = normalize_path(link_path);
        let link_parent = parent_path(&normalized_link_path);
        let absolute_target = if target.starts_with('/') {
            normalize_path(target)
        } else {
            normalize_path(&format!("{link_parent}/{target}"))
        };

        let (index, relative_path) = self.resolve_index(&normalized_link_path)?;
        let (target_index, _) = self.resolve_index(&absolute_target)?;
        if index != target_index {
            return Err(VfsError::new(
                "EXDEV",
                format!("symlink across mounts: {link_path} -> {target}"),
            ));
        }
        self.ensure_writable(index, link_path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, target.len() as u64, true)?;

        self.mounts[index]
            .filesystem
            .symlink(target, &relative_path)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let (index, relative_path) = self.resolve_link_leaf_index(path)?;
        self.mounts[index].filesystem.read_link(&relative_path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let (index, relative_path) = self.resolve_link_leaf_index(path)?;
        self.mounts[index].filesystem.lstat(&relative_path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let (old_index, old_relative_path) = self.resolve_index(old_path)?;
        let (new_index, new_relative_path) = self.resolve_index(new_path)?;
        if old_index != new_index {
            return Err(VfsError::new(
                "EXDEV",
                format!("link across mounts: {old_path} -> {new_path}"),
            ));
        }
        self.ensure_writable(new_index, new_path)?;

        self.mounts[old_index]
            .filesystem
            .link(&old_relative_path, &new_relative_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        self.mounts[index].filesystem.chmod(&relative_path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.chown_spec(path, uid, gid, true)
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let (index, relative_path) = if follow_symlinks {
            self.resolve_index(path)?
        } else {
            self.resolve_link_leaf_index(path)?
        };
        self.ensure_writable(index, path)?;
        self.mounts[index]
            .filesystem
            .chown_spec(&relative_path, uid, gid, follow_symlinks)
    }

    fn lchown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_link_leaf_index(path)?;
        self.ensure_writable(index, path)?;
        self.mounts[index]
            .filesystem
            .lchown(&relative_path, uid, gid)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        let (index, relative_path) = if follow_symlinks {
            self.resolve_content_index(path)?
        } else {
            self.resolve_link_leaf_index(path)?
        };
        self.mounts[index]
            .filesystem
            .get_xattr(&relative_path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let (index, relative_path) = if follow_symlinks {
            self.resolve_content_index(path)?
        } else {
            self.resolve_link_leaf_index(path)?
        };
        self.mounts[index]
            .filesystem
            .list_xattrs(&relative_path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let (index, relative_path) = if follow_symlinks {
            self.resolve_content_index(path)?
        } else {
            self.resolve_link_leaf_index(path)?
        };
        self.ensure_writable(index, path)?;
        self.mounts[index]
            .filesystem
            .set_xattr(&relative_path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        let (index, relative_path) = if follow_symlinks {
            self.resolve_content_index(path)?
        } else {
            self.resolve_link_leaf_index(path)?
        };
        self.ensure_writable(index, path)?;
        self.mounts[index]
            .filesystem
            .remove_xattr(&relative_path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        self.mounts[index]
            .filesystem
            .utimes(&relative_path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        self.mounts[index]
            .filesystem
            .utimes_spec(&relative_path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_writable_index(path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        self.check_file_growth(index, &relative_path, length, false)?;
        self.mounts[index]
            .filesystem
            .truncate(&relative_path, length)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }

    fn sync(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.mounts[index].filesystem.sync(&relative_path)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path)?;
        let current_size = before.size;
        self.check_file_growth(
            index,
            &relative_path,
            current_size.max(offset.saturating_add(length)),
            false,
        )?;
        self.mounts[index]
            .filesystem
            .allocate(&relative_path, offset, length)?;
        self.update_cached_path_usage(index, Some(before), &relative_path);
        Ok(())
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path)?;
        let current_size = before.size;
        self.check_file_growth(
            index,
            &relative_path,
            current_size.saturating_add(length),
            false,
        )?;
        self.mounts[index]
            .filesystem
            .insert_range(&relative_path, offset, length)?;
        self.update_cached_path_usage(index, Some(before), &relative_path);
        Ok(())
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path)?;
        self.mounts[index]
            .filesystem
            .collapse_range(&relative_path, offset, length)?;
        self.update_cached_path_usage(index, Some(before), &relative_path);
        Ok(())
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path)?;
        if !keep_size {
            let current_size = before.size;
            self.check_file_growth(
                index,
                &relative_path,
                current_size.max(offset.saturating_add(length)),
                false,
            )?;
        }
        self.mounts[index]
            .filesystem
            .zero_range(&relative_path, offset, length, keep_size)?;
        self.update_cached_path_usage(index, Some(before), &relative_path);
        Ok(())
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        self.mounts[index]
            .filesystem
            .punch_hole(&relative_path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.mounts[index]
            .filesystem
            .allocated_ranges(&relative_path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.mounts[index]
            .filesystem
            .unwritten_ranges(&relative_path)
    }

    fn extent_at(&mut self, path: &str, extent_index: usize) -> VfsResult<Option<FileExtent>> {
        let (mount_index, relative_path) = self.resolve_content_index(path)?;
        self.mounts[mount_index]
            .filesystem
            .extent_at(&relative_path, extent_index)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let (index, relative_path) = self.resolve_content_index(path)?;
        let before = self.atime_snapshot(index, &relative_path, false)?;
        let content = self.mounts[index]
            .filesystem
            .pread(&relative_path, offset, length)?;
        self.finish_atime_update(index, &relative_path, before)?;
        Ok(content)
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        let content = content.into();
        let (index, relative_path) = self.resolve_content_index(path)?;
        self.ensure_writable(index, path)?;
        let before = self.mounts[index].filesystem.lstat(&relative_path).ok();
        let current_size = before.as_ref().map_or(0, |stat| stat.size);
        self.check_file_growth(
            index,
            &relative_path,
            current_size.max(offset.saturating_add(content.len() as u64)),
            false,
        )?;
        self.mounts[index]
            .filesystem
            .pwrite(&relative_path, content, offset)?;
        self.update_cached_path_usage(index, before, &relative_path);
        Ok(())
    }
}

fn parse_mount_limit(option: &str, name: &str) -> VfsResult<u64> {
    let value = option
        .strip_prefix(&format!("{name}="))
        .ok_or_else(|| VfsError::new("EINVAL", format!("invalid mount option: {option}")))?;
    let parsed = value.parse::<u64>().map_err(|_| {
        VfsError::new(
            "EINVAL",
            format!("mount option {name} requires an unsigned integer"),
        )
    })?;
    if parsed == 0 {
        return Err(VfsError::new(
            "EINVAL",
            format!("mount option {name} must be greater than zero"),
        ));
    }
    Ok(parsed)
}

fn check_usage_limits(
    usage: &FileSystemUsage,
    max_bytes: Option<u64>,
    max_inodes: Option<usize>,
) -> VfsResult<()> {
    if max_bytes.is_some_and(|limit| usage.total_bytes > limit) {
        return Err(VfsError::new(
            "ENOSPC",
            format!(
                "filesystem byte limit exceeded: {} bytes used",
                usage.total_bytes
            ),
        ));
    }
    if max_inodes.is_some_and(|limit| usage.inode_count > limit) {
        return Err(VfsError::new(
            "ENOSPC",
            format!(
                "filesystem inode limit exceeded: {} inodes used",
                usage.inode_count
            ),
        ));
    }
    Ok(())
}

fn normalize_path(path: &str) -> String {
    let mut segments = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::RootDir => segments.clear(),
            Component::ParentDir => {
                segments.pop();
            }
            Component::CurDir => {}
            Component::Normal(value) => segments.push(value.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                segments.push(prefix.as_os_str().to_string_lossy().into_owned());
            }
        }
    }

    if segments.is_empty() {
        String::from("/")
    } else {
        format!("/{}", segments.join("/"))
    }
}

fn timestamp_ns(milliseconds: u64, nanoseconds: u32) -> u128 {
    u128::from(milliseconds) * 1_000_000 + u128::from(nanoseconds % 1_000_000)
}

fn path_components(path: &str) -> VecDeque<String> {
    normalize_path(path)
        .split('/')
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect()
}

fn join_path(parent: &str, child: &str) -> String {
    if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

fn parent_path(path: &str) -> String {
    let normalized = normalize_path(path);
    let parent = Path::new(&normalized)
        .parent()
        .unwrap_or_else(|| Path::new("/"));
    let value = parent.to_string_lossy();
    if value.is_empty() {
        String::from("/")
    } else {
        value.into_owned()
    }
}

fn basename(path: &str) -> String {
    let normalized = normalize_path(path);
    Path::new(&normalized)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("/"))
}
