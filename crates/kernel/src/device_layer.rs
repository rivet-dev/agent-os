use crate::vfs::{
    FileExtent, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat,
    VirtualUtimeSpec,
};
use getrandom::getrandom;
use web_time::{SystemTime, UNIX_EPOCH};

const DEVICE_PATHS: &[&str] = &[
    "/dev/null",
    "/dev/zero",
    "/dev/stdin",
    "/dev/stdout",
    "/dev/stderr",
    "/dev/urandom",
];

const DEVICE_DIRS: &[&str] = &["/dev/fd", "/dev/pts"];
const DEFAULT_STREAM_DEVICE_READ_BYTES: usize = 4096;
const DEV_DIR_ENTRIES: &[(&str, bool)] = &[
    ("null", false),
    ("zero", false),
    ("stdin", false),
    ("stdout", false),
    ("stderr", false),
    ("urandom", false),
    ("fd", true),
];

#[derive(Debug, Clone)]
pub struct DeviceLayer<V> {
    inner: V,
}

pub fn create_device_layer<V>(vfs: V) -> DeviceLayer<V> {
    DeviceLayer { inner: vfs }
}

impl<V> DeviceLayer<V> {
    pub fn into_inner(self) -> V {
        self.inner
    }

    pub fn inner(&self) -> &V {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut V {
        &mut self.inner
    }
}

impl<V: VirtualFileSystem> VirtualFileSystem for DeviceLayer<V> {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        if let Some(bytes) = read_stream_device(path, DEFAULT_STREAM_DEVICE_READ_BYTES) {
            return bytes;
        }

        if self
            .inner
            .stat(path)
            .is_ok_and(|stat| is_null_character_device(&stat))
        {
            return Ok(Vec::new());
        }

        self.inner.read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        if path == "/dev" {
            return Ok(DEV_DIR_ENTRIES
                .iter()
                .map(|(name, _)| String::from(*name))
                .collect());
        }
        if DEVICE_DIRS.contains(&path) {
            return Ok(Vec::new());
        }
        self.inner.read_dir(path)
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        if path == "/dev" {
            let entries = DEV_DIR_ENTRIES
                .iter()
                .map(|(name, _)| String::from(*name))
                .collect::<Vec<_>>();
            if entries.len() > max_entries {
                return Err(VfsError::new(
                    "ENOMEM",
                    format!(
                        "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                    ),
                ));
            }
            return Ok(entries);
        }
        if DEVICE_DIRS.contains(&path) {
            return Ok(Vec::new());
        }
        self.inner.read_dir_limited(path, max_entries)
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        if path == "/dev" {
            return Ok(DEV_DIR_ENTRIES
                .iter()
                .map(|(name, is_directory)| VirtualDirEntry {
                    name: String::from(*name),
                    is_directory: *is_directory,
                    is_symbolic_link: false,
                })
                .collect());
        }
        if DEVICE_DIRS.contains(&path) {
            return Ok(Vec::new());
        }
        self.inner.read_dir_with_types(path)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        if is_sink_device_path(path)
            || self
                .inner
                .stat(path)
                .is_ok_and(|stat| is_null_character_device(&stat))
        {
            let _ = content.into();
            return Ok(());
        }
        self.inner.write_file(path, content)
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            let _ = content.into();
            return Err(VfsError::new(
                "EEXIST",
                format!("file already exists, open '{path}'"),
            ));
        }
        self.inner.create_file_exclusive(path, content)
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        if is_sink_device_path(path)
            || self
                .inner
                .stat(path)
                .is_ok_and(|stat| is_null_character_device(&stat))
        {
            return Ok(content.into().len() as u64);
        }
        self.inner.append_file(path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        if is_device_dir(path) {
            return Ok(());
        }
        self.inner.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        if is_device_dir(path) {
            return Ok(());
        }
        self.inner.mkdir(path, recursive)
    }

    fn mknod(&mut self, path: &str, mode: u32, rdev: u64) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EEXIST",
                format!("device already exists: {path}"),
            ));
        }
        self.inner.mknod(path, mode, rdev)
    }

    fn exists(&self, path: &str) -> bool {
        if is_device_path(path) || is_device_dir(path) {
            return true;
        }
        self.inner.exists(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        if is_device_path(path) {
            return Ok(device_stat(path));
        }
        if is_device_dir(path) {
            return Ok(device_dir_stat(path));
        }
        self.inner.stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        if is_device_path(path) {
            return Err(VfsError::permission_denied("unlink", path));
        }
        self.inner.remove_file(path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        if is_device_dir(path) {
            return Err(VfsError::permission_denied("rmdir", path));
        }
        self.inner.remove_dir(path)
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        if is_device_path(old_path) || is_device_path(new_path) {
            return Err(VfsError::permission_denied("rename", old_path));
        }
        self.inner.rename(old_path, new_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        if is_device_path(path) || is_device_dir(path) {
            return Ok(String::from(path));
        }
        self.inner.realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.inner.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        self.inner.read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        if is_device_path(path) {
            return Ok(device_stat(path));
        }
        if is_device_dir(path) {
            return Ok(device_dir_stat(path));
        }
        self.inner.lstat(path)
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        if is_device_path(old_path) {
            return Err(VfsError::permission_denied("link", old_path));
        }
        self.inner.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        if is_device_path(path) {
            return Ok(());
        }
        self.inner.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        if is_device_path(path) {
            return Ok(());
        }
        self.inner.chown(path, uid, gid)
    }

    fn chown_spec(
        &mut self,
        path: &str,
        uid: u32,
        gid: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if is_device_path(path) {
            return Ok(());
        }
        self.inner.chown_spec(path, uid, gid, follow_symlinks)
    }

    fn get_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<Vec<u8>> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EOPNOTSUPP",
                format!("extended attributes are not supported for device {path}"),
            ));
        }
        self.inner.get_xattr(path, name, follow_symlinks)
    }

    fn list_xattrs(&mut self, path: &str, follow_symlinks: bool) -> VfsResult<Vec<String>> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EOPNOTSUPP",
                format!("extended attributes are not supported for device {path}"),
            ));
        }
        self.inner.list_xattrs(path, follow_symlinks)
    }

    fn set_xattr(
        &mut self,
        path: &str,
        name: &str,
        value: Vec<u8>,
        flags: u32,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EOPNOTSUPP",
                format!("extended attributes are not supported for device {path}"),
            ));
        }
        self.inner
            .set_xattr(path, name, value, flags, follow_symlinks)
    }

    fn remove_xattr(&mut self, path: &str, name: &str, follow_symlinks: bool) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EOPNOTSUPP",
                format!("extended attributes are not supported for device {path}"),
            ));
        }
        self.inner.remove_xattr(path, name, follow_symlinks)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        if is_device_path(path) {
            return Ok(());
        }
        self.inner.utimes(path, atime_ms, mtime_ms)
    }

    fn utimes_spec(
        &mut self,
        path: &str,
        atime: VirtualUtimeSpec,
        mtime: VirtualUtimeSpec,
        follow_symlinks: bool,
    ) -> VfsResult<()> {
        if is_device_path(path) {
            return Ok(());
        }
        self.inner.utimes_spec(path, atime, mtime, follow_symlinks)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        if is_sink_device_path(path)
            || self
                .inner
                .stat(path)
                .is_ok_and(|stat| is_null_character_device(&stat))
        {
            let _ = length;
            return Ok(());
        }
        self.inner.truncate(path, length)
    }

    fn sync(&mut self, path: &str) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support sync: {path}"),
            ));
        }
        self.inner.sync(path)
    }

    fn allocate(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support allocation: {path}"),
            ));
        }
        self.inner.allocate(path, offset, length)
    }

    fn insert_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support insert range: {path}"),
            ));
        }
        self.inner.insert_range(path, offset, length)
    }

    fn collapse_range(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support collapse range: {path}"),
            ));
        }
        self.inner.collapse_range(path, offset, length)
    }

    fn zero_range(
        &mut self,
        path: &str,
        offset: u64,
        length: u64,
        keep_size: bool,
    ) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support zero range: {path}"),
            ));
        }
        self.inner.zero_range(path, offset, length, keep_size)
    }

    fn punch_hole(&mut self, path: &str, offset: u64, length: u64) -> VfsResult<()> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support hole punching: {path}"),
            ));
        }
        self.inner.punch_hole(path, offset, length)
    }

    fn allocated_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support extent mapping: {path}"),
            ));
        }
        self.inner.allocated_ranges(path)
    }

    fn unwritten_ranges(&mut self, path: &str) -> VfsResult<Vec<(u64, u64)>> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support extent mapping: {path}"),
            ));
        }
        self.inner.unwritten_ranges(path)
    }

    fn extent_at(&mut self, path: &str, index: usize) -> VfsResult<Option<FileExtent>> {
        if is_device_path(path) || is_device_dir(path) {
            return Err(VfsError::new(
                "EINVAL",
                format!("device does not support extent mapping: {path}"),
            ));
        }
        self.inner.extent_at(path, index)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        if let Some(bytes) = read_stream_device(path, length) {
            return bytes;
        }

        if self
            .inner
            .stat(path)
            .is_ok_and(|stat| is_null_character_device(&stat))
        {
            return Ok(Vec::new());
        }

        self.inner.pread(path, offset, length)
    }

    fn pwrite(&mut self, path: &str, content: impl Into<Vec<u8>>, offset: u64) -> VfsResult<()> {
        if is_sink_device_path(path)
            || self
                .inner
                .stat(path)
                .is_ok_and(|stat| is_null_character_device(&stat))
        {
            let _ = (content.into(), offset);
            return Ok(());
        }
        self.inner.pwrite(path, content, offset)
    }
}

fn is_device_path(path: &str) -> bool {
    DEVICE_PATHS.contains(&path) || path.starts_with("/dev/fd/") || path.starts_with("/dev/pts/")
}

/// Standard emulated character devices (`/dev/null`, `/dev/zero`, `/dev/urandom`,
/// `/dev/std{in,out,err}`). On Linux these are world-readable/writable and have no
/// host backing — they are pure kernel emulations whose semantics this device layer
/// already enforces (read/write for the stream devices, `EPERM` for unlink/rename).
/// The permission layer therefore treats them as always accessible, so guest fs ops
/// (`readFileSync`/`existsSync`/redirects on `/dev/null`, …) behave like native Linux
/// regardless of the VM file-permission policy. Excludes `/dev/fd` and `/dev/pts`,
/// which carry process-specific semantics the policy may legitimately govern.
pub fn is_standard_device_path(path: &str) -> bool {
    DEVICE_PATHS.contains(&path)
}

fn is_sink_device_path(path: &str) -> bool {
    matches!(
        path,
        "/dev/null" | "/dev/zero" | "/dev/stdout" | "/dev/stderr" | "/dev/urandom"
    )
}

fn is_device_dir(path: &str) -> bool {
    path == "/dev" || DEVICE_DIRS.contains(&path)
}

fn device_stat(path: &str) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: 0o020666,
        size: 0,
        blocks: 0,
        dev: 2,
        rdev: device_rdev(path),
        is_directory: false,
        is_symbolic_link: false,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino: device_ino(path),
        nlink: 1,
        uid: 0,
        gid: 0,
    }
}

fn device_dir_stat(path: &str) -> VirtualStat {
    let now = now_ms();
    VirtualStat {
        mode: 0o040755,
        size: 0,
        blocks: 0,
        dev: 2,
        rdev: 0,
        is_directory: true,
        is_symbolic_link: false,
        atime_ms: now,
        atime_nsec: 0,
        mtime_ms: now,
        mtime_nsec: 0,
        ctime_ms: now,
        ctime_nsec: 0,
        birthtime_ms: now,
        ino: device_ino(path),
        nlink: 2,
        uid: 0,
        gid: 0,
    }
}

fn device_ino(path: &str) -> u64 {
    match path {
        "/dev/null" => 0xffff_0001,
        "/dev/zero" => 0xffff_0002,
        "/dev/stdin" => 0xffff_0003,
        "/dev/stdout" => 0xffff_0004,
        "/dev/stderr" => 0xffff_0005,
        "/dev/urandom" => 0xffff_0006,
        _ => 0xffff_0000,
    }
}

fn device_rdev(path: &str) -> u64 {
    match path {
        "/dev/null" => encode_device_id(1, 3),
        "/dev/zero" => encode_device_id(1, 5),
        "/dev/stdin" => encode_device_id(5, 0),
        "/dev/stdout" => encode_device_id(5, 1),
        "/dev/stderr" => encode_device_id(5, 2),
        "/dev/urandom" => encode_device_id(1, 9),
        _ => 0,
    }
}

fn encode_device_id(major: u64, minor: u64) -> u64 {
    (major << 8) | minor
}

fn is_null_character_device(stat: &VirtualStat) -> bool {
    stat.mode & 0o170000 == 0o020000 && stat.rdev == encode_device_id(1, 3)
}

fn random_bytes(length: usize) -> VfsResult<Vec<u8>> {
    let mut buffer = vec![0; length];
    getrandom(&mut buffer)
        .map_err(|error| VfsError::io(format!("failed to read system random bytes: {error}")))?;
    Ok(buffer)
}

fn read_stream_device(path: &str, length: usize) -> Option<VfsResult<Vec<u8>>> {
    match path {
        "/dev/null" => Some(Ok(Vec::new())),
        "/dev/zero" => Some(Ok(vec![0; length])),
        "/dev/urandom" => Some(random_bytes(length)),
        _ => None,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
