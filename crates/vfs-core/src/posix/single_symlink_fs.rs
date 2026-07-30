use super::vfs::{
    VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat, VirtualUtimeSpec, S_IFLNK,
};

/// Tiny read-only filesystem whose root inode is a single symbolic link.
///
/// Package projection uses this for managed leaves such as
/// `/opt/agentos/bin/<cmd>` and `/opt/agentos/pkgs/<pkg>/current`. A normal
/// `MemoryFileSystem` cannot model that shape because its root inode is always
/// a directory. Keeping each managed symlink as its own leaf mount lets the
/// parent directories remain writable overlay directories, so user-installed
/// commands and packages can coexist beside managed entries.
#[derive(Debug, Clone)]
pub struct SingleSymlinkFileSystem {
    target: String,
}

impl SingleSymlinkFileSystem {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    pub fn target(&self) -> &str {
        &self.target
    }

    fn root_stat(&self) -> VirtualStat {
        VirtualStat {
            mode: S_IFLNK | 0o777,
            size: self.target.len() as u64,
            blocks: 0,
            dev: 9101,
            rdev: 0,
            is_directory: false,
            is_symbolic_link: true,
            atime_ms: 0,
            atime_nsec: 0,
            mtime_ms: 0,
            mtime_nsec: 0,
            ctime_ms: 0,
            ctime_nsec: 0,
            birthtime_ms: 0,
            ino: 1,
            nlink: 1,
            uid: 1000,
            gid: 1000,
        }
    }

    fn not_found(path: &str) -> VfsError {
        VfsError::new("ENOENT", format!("no such file or directory, '{path}'"))
    }

    fn readonly(op: &str, path: &str) -> VfsError {
        VfsError::new(
            "EROFS",
            format!("read-only single-symlink filesystem, {op} '{path}'"),
        )
    }
}

impl VirtualFileSystem for SingleSymlinkFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        Err(Self::not_found(path))
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        Err(VfsError::new(
            "ENOTDIR",
            format!("not a directory, readdir '{path}'"),
        ))
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        Err(VfsError::new(
            "ENOTDIR",
            format!("not a directory, readdir '{path}'"),
        ))
    }

    fn write_file(&mut self, path: &str, _content: impl Into<Vec<u8>>) -> VfsResult<()> {
        Err(Self::readonly("write", path))
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly("mkdir", path))
    }

    fn mkdir(&mut self, path: &str, _recursive: bool) -> VfsResult<()> {
        Err(Self::readonly("mkdir", path))
    }

    fn exists(&self, path: &str) -> bool {
        path == "/"
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        self.lstat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly("unlink", path))
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        Err(Self::readonly("rmdir", path))
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only single-symlink filesystem, rename '{old_path}' to '{new_path}'"),
        ))
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        if path == "/" {
            return Err(VfsError::new(
                "ELOOP",
                "single-symlink root must be resolved by the mount table",
            ));
        }
        Err(Self::not_found(path))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only single-symlink filesystem, symlink '{link_path}' -> '{target}'"),
        ))
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        if path == "/" {
            Ok(self.target.clone())
        } else {
            Err(Self::not_found(path))
        }
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        if path == "/" {
            Ok(self.root_stat())
        } else {
            Err(Self::not_found(path))
        }
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        Err(VfsError::new(
            "EROFS",
            format!("read-only single-symlink filesystem, link '{old_path}' to '{new_path}'"),
        ))
    }

    fn chmod(&mut self, path: &str, _mode: u32) -> VfsResult<()> {
        Err(Self::readonly("chmod", path))
    }

    fn chown(&mut self, path: &str, _uid: u32, _gid: u32) -> VfsResult<()> {
        Err(Self::readonly("chown", path))
    }

    fn utimes(&mut self, path: &str, _atime_ms: u64, _mtime_ms: u64) -> VfsResult<()> {
        Err(Self::readonly("utimes", path))
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        _atime: VirtualUtimeSpec,
        _mtime: VirtualUtimeSpec,
        _follow_symlinks: bool,
    ) -> VfsResult<()> {
        Err(Self::readonly("utimes", path))
    }

    fn truncate(&mut self, path: &str, _length: u64) -> VfsResult<()> {
        Err(Self::readonly("truncate", path))
    }

    fn pread(&mut self, path: &str, _offset: u64, _length: usize) -> VfsResult<Vec<u8>> {
        Err(Self::not_found(path))
    }
}
