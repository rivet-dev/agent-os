use crate::engine::error::{VfsError, VfsResult};
use crate::engine::types::{Dentry, FileExtent, SnapshotId, VirtualStat};
use async_trait::async_trait;

#[async_trait]
pub trait VirtualFileSystem: Send + Sync {
    async fn read_file(&self, path: &str) -> VfsResult<Vec<u8>>;

    async fn read_text(&self, path: &str) -> VfsResult<String> {
        String::from_utf8(self.read_file(path).await?)
            .map_err(|_| VfsError::einval(format!("file is not valid UTF-8: {path}")))
    }

    async fn read_dir(&self, path: &str) -> VfsResult<Vec<String>>;
    async fn read_dir_with_types(&self, path: &str) -> VfsResult<Vec<Dentry>>;
    async fn write_file(&self, path: &str, content: &[u8]) -> VfsResult<()>;
    async fn create_dir(&self, path: &str) -> VfsResult<()>;
    async fn mkdir(&self, path: &str, recursive: bool) -> VfsResult<()>;
    async fn mknod(&self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        let _ = (mode, rdev);
        Err(VfsError::eopnotsupp(format!(
            "special inode creation is not supported for {path}"
        )))
    }
    async fn exists(&self, path: &str) -> bool;
    async fn stat(&self, path: &str) -> VfsResult<VirtualStat>;
    async fn lstat(&self, path: &str) -> VfsResult<VirtualStat>;
    async fn remove_file(&self, path: &str) -> VfsResult<()>;
    async fn remove_dir(&self, path: &str) -> VfsResult<()>;
    async fn rename(&self, old_path: &str, new_path: &str) -> VfsResult<()>;
    async fn realpath(&self, path: &str) -> VfsResult<String>;
    async fn symlink(&self, target: &str, link_path: &str) -> VfsResult<()>;
    async fn readlink(&self, path: &str) -> VfsResult<String>;
    async fn link(&self, old_path: &str, new_path: &str) -> VfsResult<()>;
    async fn chmod(&self, path: &str, mode: u32) -> VfsResult<()>;
    async fn chown(&self, path: &str, uid: u32, gid: u32) -> VfsResult<()>;
    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        self.chown(path, uid, gid).await
    }
    async fn get_xattr(&self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        let _ = (name, follow_symlinks);
        Err(VfsError::eopnotsupp(format!(
            "extended attributes are not supported for {path}"
        )))
    }
    async fn list_xattrs(&self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        let _ = follow_symlinks;
        Err(VfsError::eopnotsupp(format!(
            "extended attributes are not supported for {path}"
        )))
    }
    async fn set_xattr(
        &self,
        path: &str,
        name: &str,
        value: &[u8],
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        let _ = (name, value, flags, follow_symlinks);
        Err(VfsError::eopnotsupp(format!(
            "extended attributes are not supported for {path}"
        )))
    }
    async fn remove_xattr(&self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        let _ = (name, follow_symlinks);
        Err(VfsError::eopnotsupp(format!(
            "extended attributes are not supported for {path}"
        )))
    }
    async fn utimes(&self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()>;
    /// Update access time as a read side effect without changing ctime or mtime.
    async fn set_atime(&self, path: &str, atime_ms: u64) -> VfsResult<()> {
        let stat = self.stat(path).await?;
        let mtime_ms = if stat.mtime.sec < 0 {
            0
        } else {
            (stat.mtime.sec as u64)
                .saturating_mul(1_000)
                .saturating_add(u64::from(stat.mtime.nsec / 1_000_000))
        };
        self.utimes(path, atime_ms, mtime_ms).await
    }
    async fn truncate(&self, path: &str, length: u64) -> VfsResult<()>;
    async fn allocate(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        const ALLOCATION_CHUNK_BYTES: u64 = 64 * 1024;

        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("allocation range overflows"))?;
        if length == 0 {
            return Ok(());
        }
        let stat = self.stat(path).await?;
        if end > stat.size {
            self.truncate(path, end).await?;
        }
        let mut cursor = offset;
        while cursor < end {
            let chunk_len = (end - cursor).min(ALLOCATION_CHUNK_BYTES);
            let chunk_len = usize::try_from(chunk_len)
                .map_err(|_| VfsError::einval("allocation chunk is too large"))?;
            let mut bytes = self.pread(path, cursor, chunk_len).await?;
            bytes.resize(chunk_len, 0);
            self.pwrite(path, &bytes, cursor).await?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    async fn insert_range(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let size = self.stat(path).await?.size;
        if offset >= size {
            return Err(VfsError::einval("insert range offset must be before EOF"));
        }
        let tail_len = usize::try_from(size - offset)
            .map_err(|_| VfsError::einval("insert range tail is too large"))?;
        let tail = self.pread(path, offset, tail_len).await?;
        self.truncate(
            path,
            size.checked_add(length)
                .ok_or_else(|| VfsError::einval("insert range size overflows"))?,
        )
        .await?;
        self.pwrite(path, &tail, offset + length).await?;
        self.punch_hole(path, offset, length).await
    }
    async fn collapse_range(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        validate_shift_range(offset, length)?;
        let size = self.stat(path).await?.size;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("collapse range overflows"))?;
        if end >= size {
            return Err(VfsError::einval("collapse range must end before EOF"));
        }
        let tail_len = usize::try_from(size - end)
            .map_err(|_| VfsError::einval("collapse range tail is too large"))?;
        let tail = self.pread(path, end, tail_len).await?;
        self.pwrite(path, &tail, offset).await?;
        self.truncate(path, size - length).await
    }
    /// Zeroes and allocates a byte range, optionally preserving the file size.
    async fn zero_range(
        &self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("zero range overflows"))?;
        if length == 0 {
            return Err(VfsError::einval("zero range length must be nonzero"));
        }
        let original_size = self.stat(path).await?.size;
        self.allocate(path, offset, length).await?;
        let zero_end = if keep_size {
            end.min(original_size)
        } else {
            end
        };
        let mut cursor = offset.min(zero_end);
        while cursor < zero_end {
            let chunk_len = usize::try_from((zero_end - cursor).min(64 * 1024))
                .map_err(|_| VfsError::einval("zero range chunk is too large"))?;
            self.pwrite(path, &vec![0; chunk_len], cursor).await?;
            cursor += chunk_len as u64;
        }
        if keep_size && self.stat(path).await?.size != original_size {
            self.truncate(path, original_size).await?;
        }
        Ok(())
    }
    /// Deallocates a byte range while preserving the file size. Bytes in the
    /// intersecting range read back as zeroes.
    async fn punch_hole(&self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        const PUNCH_CHUNK_BYTES: u64 = 64 * 1024;

        let requested_end = offset
            .checked_add(length)
            .ok_or_else(|| VfsError::einval("hole-punch range overflows"))?;
        let size = self.stat(path).await?.size;
        let end = requested_end.min(size);
        let mut cursor = offset.min(size);
        while cursor < end {
            let chunk_len = (end - cursor).min(PUNCH_CHUNK_BYTES) as usize;
            self.pwrite(path, &vec![0; chunk_len], cursor).await?;
            cursor += chunk_len as u64;
        }
        Ok(())
    }
    /// Returns allocated byte ranges as half-open `(start, end)` intervals.
    async fn allocated_ranges(&self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Err(VfsError::eopnotsupp(format!(
            "extent mapping is not supported for {path}"
        )))
    }
    /// Returns unwritten allocated byte ranges as half-open `(start, end)` intervals.
    async fn unwritten_ranges(&self, _path: &str) -> VfsResult<Vec<(u64, u64)>> {
        Ok(Vec::new())
    }
    /// Returns one allocated extent, split at written/unwritten boundaries.
    async fn extent_at(&self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        let allocated = self.allocated_ranges(path).await?;
        let unwritten = self.unwritten_ranges(path).await?;
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
    async fn pread(&self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>>;
    async fn pwrite(&self, path: &str, content: &[u8], offset: u64) -> VfsResult<()>;
    async fn append(&self, path: &str, content: &[u8]) -> VfsResult<u64>;

    async fn sync(&self, _path: &str) -> VfsResult<()> {
        Ok(())
    }

    async fn shutdown(&self) -> VfsResult<()> {
        Ok(())
    }
}

fn validate_shift_range(offset: u64, length: u64) -> VfsResult<()> {
    const ALIGNMENT: u64 = 512;
    if length == 0 || !offset.is_multiple_of(ALIGNMENT) || !length.is_multiple_of(ALIGNMENT) {
        return Err(VfsError::einval(
            "insert/collapse range requires a nonzero 512-byte-aligned range",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait Snapshottable: Send + Sync {
    async fn snapshot(&self, root: u64) -> VfsResult<SnapshotId>;
    async fn fork(&self, snap: SnapshotId) -> VfsResult<u64>;
}
