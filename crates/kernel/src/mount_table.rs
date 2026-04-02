use crate::vfs::{VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem, VirtualStat};
use std::any::Any;
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub trait MountedFileSystem: Any {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>>;
    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>>;
    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>>;
    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()>;
    fn create_dir(&mut self, path: &str) -> VfsResult<()>;
    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()>;
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
    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()>;
    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()>;
    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>>;
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

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        VirtualFileSystem::read_dir_with_types(&mut self.inner, path)
    }

    fn write_file(&mut self, path: &str, content: Vec<u8>) -> VfsResult<()> {
        VirtualFileSystem::write_file(&mut self.inner, path, content)
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        VirtualFileSystem::create_dir(&mut self.inner, path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        VirtualFileSystem::mkdir(&mut self.inner, path, recursive)
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

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        VirtualFileSystem::utimes(&mut self.inner, path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        VirtualFileSystem::truncate(&mut self.inner, path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        VirtualFileSystem::pread(&mut self.inner, path, offset, length)
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

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        (**self).utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        (**self).truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        (**self).pread(path, offset, length)
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

    fn utimes(&mut self, path: &str, _atime_ms: u64, _mtime_ms: u64) -> VfsResult<()> {
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

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        self.inner.pread(path, offset, length)
    }

    fn shutdown(&mut self) -> VfsResult<()> {
        self.inner.shutdown()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry {
    pub path: String,
    pub plugin_id: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountOptions {
    pub plugin_id: String,
    pub read_only: bool,
}

impl MountOptions {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            read_only: false,
        }
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }
}

struct MountRegistration {
    path: String,
    plugin_id: String,
    read_only: bool,
    filesystem: Box<dyn MountedFileSystem>,
}

pub struct MountTable {
    mounts: Vec<MountRegistration>,
}

impl MountTable {
    pub fn new(root_fs: impl VirtualFileSystem + 'static) -> Self {
        Self {
            mounts: vec![MountRegistration {
                path: String::from("/"),
                plugin_id: String::from("root"),
                read_only: false,
                filesystem: Box::new(MountedVirtualFileSystem::new(root_fs)),
            }],
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
        filesystem: Box<dyn MountedFileSystem>,
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
            let _ = parent_mount.filesystem.mkdir(&relative_path, true);
        }

        let filesystem = if options.read_only {
            Box::new(ReadOnlyFileSystem::new(filesystem)) as Box<dyn MountedFileSystem>
        } else {
            filesystem
        };

        self.mounts.push(MountRegistration {
            path: normalized,
            plugin_id: options.plugin_id,
            read_only: options.read_only,
            filesystem,
        });
        self.mounts
            .sort_by(|left, right| right.path.len().cmp(&left.path.len()));
        Ok(())
    }

    pub fn unmount(&mut self, path: &str) -> VfsResult<()> {
        let normalized = normalize_path(path);
        if normalized == "/" {
            return Err(VfsError::new("EINVAL", "cannot unmount root"));
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
        mount.filesystem.shutdown()?;
        Ok(())
    }

    pub fn get_mounts(&self) -> Vec<MountEntry> {
        self.mounts
            .iter()
            .map(|mount| MountEntry {
                path: mount.path.clone(),
                plugin_id: mount.plugin_id.clone(),
                read_only: mount.read_only,
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

    fn resolve_index(&self, full_path: &str) -> VfsResult<(usize, String)> {
        let normalized = normalize_path(full_path);
        for (index, mount) in self.mounts.iter().enumerate() {
            if mount.path == "/" {
                return Ok((index, normalized));
            }
            if normalized == mount.path {
                return Ok((index, String::from("/")));
            }
            if normalized.starts_with(&format!("{}/", mount.path)) {
                let suffix = normalized
                    .trim_start_matches(&mount.path)
                    .trim_start_matches('/');
                return Ok((index, format!("/{suffix}")));
            }
        }

        Err(VfsError::new(
            "ENOENT",
            format!("no such file or directory, resolve '{full_path}'"),
        ))
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
}

impl Drop for MountTable {
    fn drop(&mut self) {
        for mount in self.mounts.iter_mut().rev() {
            let _ = mount.filesystem.shutdown();
        }
    }
}

impl VirtualFileSystem for MountTable {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.read_file(&relative_path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        let normalized = normalize_path(path);
        let (index, relative_path) = self.resolve_index(&normalized)?;
        let mut entries = self.mounts[index].filesystem.read_dir(&relative_path)?;
        let child_mounts = self.child_mount_basenames(&normalized);
        if child_mounts.is_empty() {
            return Ok(entries);
        }

        let mut merged = BTreeSet::new();
        merged.extend(entries.drain(..));
        merged.extend(child_mounts);
        Ok(merged.into_iter().collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        let normalized = normalize_path(path);
        let (index, relative_path) = self.resolve_index(&normalized)?;
        let mut entries = self.mounts[index]
            .filesystem
            .read_dir_with_types(&relative_path)?;
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
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .write_file(&relative_path, content.into())
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.create_dir(&relative_path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .mkdir(&relative_path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.resolve_index(path)
            .map(|(index, relative_path)| self.mounts[index].filesystem.exists(&relative_path))
            .unwrap_or(false)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.stat(&relative_path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.remove_file(&relative_path)
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.remove_dir(&relative_path)
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

        self.mounts[old_index]
            .filesystem
            .rename(&old_relative_path, &new_relative_path)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        let (index, relative_path) = self.resolve_index(path)?;
        let mount = &self.mounts[index];
        let resolved = mount.filesystem.realpath(&relative_path)?;
        if mount.path == "/" {
            return Ok(resolved);
        }
        if resolved == "/" {
            return Ok(mount.path.clone());
        }
        Ok(format!(
            "{}/{}",
            mount.path.trim_end_matches('/'),
            resolved.trim_start_matches('/')
        ))
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(link_path)?;
        self.mounts[index]
            .filesystem
            .symlink(target, &relative_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.read_link(&relative_path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        let (index, relative_path) = self.resolve_index(path)?;
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

        self.mounts[old_index]
            .filesystem
            .link(&old_relative_path, &new_relative_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index].filesystem.chmod(&relative_path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .chown(&relative_path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .utimes(&relative_path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .truncate(&relative_path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        let (index, relative_path) = self.resolve_index(path)?;
        self.mounts[index]
            .filesystem
            .pread(&relative_path, offset, length)
    }
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
