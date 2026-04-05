use crate::vfs::{
    normalize_path, MemoryFileSystem, VfsError, VfsResult, VirtualDirEntry, VirtualFileSystem,
    VirtualStat,
};
use std::collections::BTreeSet;

const MAX_SNAPSHOT_DEPTH: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    Ephemeral,
    ReadOnly,
}

#[derive(Debug)]
pub struct OverlayFileSystem {
    lowers: Vec<MemoryFileSystem>,
    upper: Option<MemoryFileSystem>,
    whiteouts: BTreeSet<String>,
    writes_locked: bool,
}

#[derive(Debug)]
enum OverlaySnapshotKind {
    Directory,
    File(Vec<u8>),
    Symlink(String),
}

#[derive(Debug)]
struct OverlaySnapshotEntry {
    path: String,
    stat: VirtualStat,
    kind: OverlaySnapshotKind,
}

impl OverlayFileSystem {
    pub fn new(lowers: Vec<MemoryFileSystem>, mode: OverlayMode) -> Self {
        let mut effective_lowers = lowers;
        if effective_lowers.is_empty() {
            effective_lowers.push(MemoryFileSystem::new());
        }

        let mut upper = match mode {
            OverlayMode::Ephemeral => Some(MemoryFileSystem::new()),
            OverlayMode::ReadOnly => None,
        };
        if let Some(upper_filesystem) = upper.as_mut() {
            sync_upper_root_metadata(upper_filesystem, &effective_lowers);
        }

        Self {
            lowers: effective_lowers,
            upper,
            whiteouts: BTreeSet::new(),
            writes_locked: matches!(mode, OverlayMode::ReadOnly),
        }
    }

    pub fn with_upper(lowers: Vec<MemoryFileSystem>, upper: MemoryFileSystem) -> Self {
        let mut effective_lowers = lowers;
        if effective_lowers.is_empty() {
            effective_lowers.push(MemoryFileSystem::new());
        }

        Self {
            lowers: effective_lowers,
            upper: Some(upper),
            whiteouts: BTreeSet::new(),
            writes_locked: false,
        }
    }

    pub fn lock_writes(&mut self) {
        self.writes_locked = true;
    }

    fn normalized(path: &str) -> String {
        normalize_path(path)
    }

    fn is_whited_out(&self, path: &str) -> bool {
        self.whiteouts.contains(&Self::normalized(path))
    }

    fn add_whiteout(&mut self, path: &str) {
        self.whiteouts.insert(Self::normalized(path));
    }

    fn remove_whiteout(&mut self, path: &str) {
        self.whiteouts.remove(&Self::normalized(path));
    }

    fn join_path(base: &str, name: &str) -> String {
        if base == "/" {
            format!("/{name}")
        } else {
            format!("{base}/{name}")
        }
    }

    fn rebase_path(path: &str, old_root: &str, new_root: &str) -> String {
        if path == old_root {
            return String::from(new_root);
        }

        format!("{new_root}{}", &path[old_root.len()..])
    }

    fn read_only_error(path: &str) -> VfsError {
        VfsError::new("EROFS", format!("read-only filesystem: {path}"))
    }

    fn entry_not_found(path: &str) -> VfsError {
        VfsError::new("ENOENT", format!("no such file: {path}"))
    }

    fn directory_not_found(path: &str) -> VfsError {
        VfsError::new("ENOENT", format!("no such directory: {path}"))
    }

    fn already_exists(path: &str) -> VfsError {
        VfsError::new("EEXIST", format!("file exists: {path}"))
    }

    fn not_directory(path: &str) -> VfsError {
        VfsError::new("ENOTDIR", format!("not a directory: {path}"))
    }

    fn writable_upper(&mut self, path: &str) -> VfsResult<&mut MemoryFileSystem> {
        if self.writes_locked {
            return Err(Self::read_only_error(path));
        }
        self.upper
            .as_mut()
            .ok_or_else(|| Self::read_only_error(path))
    }

    fn path_exists_in_filesystem(filesystem: &MemoryFileSystem, path: &str) -> bool {
        filesystem.exists(path)
    }

    fn has_entry_in_filesystem(filesystem: &MemoryFileSystem, path: &str) -> bool {
        filesystem.lstat(path).is_ok()
    }

    fn exists_in_upper(&self, path: &str) -> bool {
        self.upper
            .as_ref()
            .is_some_and(|upper| Self::path_exists_in_filesystem(upper, path))
    }

    fn has_entry_in_upper(&self, path: &str) -> bool {
        self.upper
            .as_ref()
            .is_some_and(|upper| Self::has_entry_in_filesystem(upper, path))
    }

    fn find_lower_by_exists(&self, path: &str) -> Option<usize> {
        self.lowers
            .iter()
            .position(|lower| Self::path_exists_in_filesystem(lower, path))
    }

    fn find_lower_by_entry(&self, path: &str) -> Option<(usize, VirtualStat)> {
        self.lowers
            .iter()
            .enumerate()
            .find_map(|(index, lower)| lower.lstat(path).ok().map(|stat| (index, stat)))
    }

    fn merged_lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.has_entry_in_upper(path) {
            return self
                .upper
                .as_ref()
                .expect("upper must exist when entry exists")
                .lstat(path);
        }
        self.find_lower_by_entry(path)
            .map(|(_, stat)| stat)
            .ok_or_else(|| Self::entry_not_found(path))
    }

    fn ensure_ancestor_directories_in_upper(&mut self, path: &str) -> VfsResult<()> {
        let normalized = Self::normalized(path);
        let parts = normalized
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();

        let mut current = String::new();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            current.push('/');
            current.push_str(part);

            if self.exists_in_upper(&current) {
                continue;
            }

            if let Some(index) = self.find_lower_by_exists(&current) {
                let stat = self.lowers[index].stat(&current)?;
                if !stat.is_directory {
                    return Err(Self::not_directory(&current));
                }

                let upper = self.writable_upper(&current)?;
                upper.mkdir(&current, false)?;
                upper.chmod(&current, stat.mode)?;
                upper.chown(&current, stat.uid, stat.gid)?;
                continue;
            }

            let upper = self.writable_upper(&current)?;
            upper.mkdir(&current, false)?;
        }

        Ok(())
    }

    fn copy_up_path(&mut self, path: &str) -> VfsResult<()> {
        if self.has_entry_in_upper(path) {
            return Ok(());
        }

        self.ensure_ancestor_directories_in_upper(path)?;

        let (lower_index, stat) = self
            .find_lower_by_entry(path)
            .ok_or_else(|| Self::entry_not_found(path))?;

        if stat.is_symbolic_link {
            let target = self.lowers[lower_index].read_link(path)?;
            let upper = self.writable_upper(path)?;
            upper.symlink(&target, path)?;
            return Ok(());
        }

        if stat.is_directory {
            let upper = self.writable_upper(path)?;
            upper.mkdir(path, false)?;
            upper.chmod(path, stat.mode)?;
            upper.chown(path, stat.uid, stat.gid)?;
            return Ok(());
        }

        let data = self.lowers[lower_index].read_file(path)?;
        let upper = self.writable_upper(path)?;
        upper.write_file(path, data)?;
        upper.chmod(path, stat.mode)?;
        upper.chown(path, stat.uid, stat.gid)?;
        Ok(())
    }

    fn path_exists_in_merged_view(&self, path: &str) -> bool {
        if self.is_whited_out(path) {
            return false;
        }
        if self.has_entry_in_upper(path) {
            return true;
        }
        self.find_lower_by_entry(path).is_some()
    }

    fn not_empty(path: &str) -> VfsError {
        VfsError::new("ENOTEMPTY", format!("directory not empty, rmdir '{path}'"))
    }

    fn remove_existing_destination(&mut self, path: &str) -> VfsResult<()> {
        let stat = match self.merged_lstat(path) {
            Ok(stat) => stat,
            Err(error) if error.code() == "ENOENT" => return Ok(()),
            Err(error) => return Err(error),
        };

        if stat.is_directory && !stat.is_symbolic_link {
            if !self.read_dir(path)?.is_empty() {
                return Err(Self::not_empty(path));
            }
            self.remove_dir(path)
        } else {
            self.remove_file(path)
        }
    }

    fn collect_snapshot_entries(
        &mut self,
        path: &str,
        entries: &mut Vec<OverlaySnapshotEntry>,
    ) -> VfsResult<()> {
        let mut pending = vec![(Self::normalized(path), 0usize)];
        while let Some((current_path, depth)) = pending.pop() {
            if depth > MAX_SNAPSHOT_DEPTH {
                return Err(VfsError::new(
                    "EINVAL",
                    format!("overlay snapshot depth limit exceeded at '{current_path}'"),
                ));
            }

            let stat = self.lstat(&current_path)?;

            if stat.is_symbolic_link {
                entries.push(OverlaySnapshotEntry {
                    path: current_path.clone(),
                    stat,
                    kind: OverlaySnapshotKind::Symlink(self.read_link(&current_path)?),
                });
                continue;
            }

            if stat.is_directory {
                entries.push(OverlaySnapshotEntry {
                    path: current_path.clone(),
                    stat,
                    kind: OverlaySnapshotKind::Directory,
                });

                let children = self.read_dir_with_types(&current_path)?;
                for entry in children.into_iter().rev() {
                    pending.push((Self::join_path(&current_path, &entry.name), depth + 1));
                }
                continue;
            }

            entries.push(OverlaySnapshotEntry {
                path: current_path.clone(),
                stat,
                kind: OverlaySnapshotKind::File(self.read_file(&current_path)?),
            });
        }
        Ok(())
    }

    fn materialize_snapshot_entries(
        &mut self,
        old_root: &str,
        new_root: &str,
        entries: &[OverlaySnapshotEntry],
    ) -> VfsResult<()> {
        for entry in entries {
            let destination = Self::rebase_path(&entry.path, old_root, new_root);

            match &entry.kind {
                OverlaySnapshotKind::Directory => {
                    self.create_dir(&destination)?;
                    self.chmod(&destination, entry.stat.mode)?;
                    self.chown(&destination, entry.stat.uid, entry.stat.gid)?;
                }
                OverlaySnapshotKind::File(data) => {
                    self.write_file(&destination, data.clone())?;
                    self.chmod(&destination, entry.stat.mode)?;
                    self.chown(&destination, entry.stat.uid, entry.stat.gid)?;
                }
                OverlaySnapshotKind::Symlink(target) => {
                    self.remove_whiteout(&destination);
                    self.ensure_ancestor_directories_in_upper(&destination)?;
                    self.writable_upper(&destination)?
                        .symlink(target, &destination)?;
                }
            }
        }

        Ok(())
    }

    fn remove_snapshot_entries(&mut self, entries: &[OverlaySnapshotEntry]) -> VfsResult<()> {
        for entry in entries.iter().rev() {
            if self.has_entry_in_upper(&entry.path) {
                match entry.kind {
                    OverlaySnapshotKind::Directory => {
                        self.writable_upper(&entry.path)?.remove_dir(&entry.path)?;
                    }
                    OverlaySnapshotKind::File(_) | OverlaySnapshotKind::Symlink(_) => {
                        self.writable_upper(&entry.path)?.remove_file(&entry.path)?;
                    }
                }
            }

            if self.find_lower_by_entry(&entry.path).is_some() {
                self.add_whiteout(&entry.path);
            } else {
                self.remove_whiteout(&entry.path);
            }
        }

        Ok(())
    }
}

fn sync_upper_root_metadata(upper: &mut MemoryFileSystem, lowers: &[MemoryFileSystem]) {
    let Some(root_stat) = lowers.iter().find_map(|lower| lower.lstat("/").ok()) else {
        return;
    };

    upper
        .chmod("/", root_stat.mode)
        .expect("overlay upper root should exist");
    upper
        .chown("/", root_stat.uid, root_stat.gid)
        .expect("overlay upper root should exist");
}

impl VirtualFileSystem for OverlayFileSystem {
    fn read_file(&mut self, path: &str) -> VfsResult<Vec<u8>> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            return self
                .upper
                .as_mut()
                .expect("upper must exist when path exists")
                .read_file(path);
        }
        let Some(index) = self.find_lower_by_exists(path) else {
            return Err(Self::entry_not_found(path));
        };
        self.lowers[index].read_file(path)
    }

    fn read_dir(&mut self, path: &str) -> VfsResult<Vec<String>> {
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = BTreeSet::new();
        let whiteouts = self.whiteouts.clone();

        for lower in self.lowers.iter_mut().rev() {
            if let Ok(lower_entries) = lower.read_dir(path) {
                directory_exists = true;
                for entry in lower_entries {
                    if entry == "." || entry == ".." {
                        continue;
                    }
                    let child_path = if normalized == "/" {
                        format!("/{entry}")
                    } else {
                        format!("{normalized}/{entry}")
                    };
                    if !whiteouts.contains(&Self::normalized(&child_path)) {
                        entries.insert(entry);
                    }
                }
            }
        }

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(upper_entries) = upper.read_dir(path) {
                directory_exists = true;
                for entry in upper_entries {
                    if entry == "." || entry == ".." {
                        continue;
                    }
                    entries.insert(entry);
                }
            }
        }

        if !directory_exists {
            return Err(Self::directory_not_found(path));
        }

        Ok(entries.into_iter().collect())
    }

    fn read_dir_limited(&mut self, path: &str, max_entries: usize) -> VfsResult<Vec<String>> {
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = BTreeSet::new();
        let whiteouts = self.whiteouts.clone();

        for lower in self.lowers.iter_mut().rev() {
            if let Ok(lower_entries) = lower.read_dir(path) {
                directory_exists = true;
                for entry in lower_entries {
                    if entry == "." || entry == ".." {
                        continue;
                    }
                    let child_path = if normalized == "/" {
                        format!("/{entry}")
                    } else {
                        format!("{normalized}/{entry}")
                    };
                    if !whiteouts.contains(&Self::normalized(&child_path)) {
                        entries.insert(entry);
                        if entries.len() > max_entries {
                            return Err(VfsError::new(
                                "ENOMEM",
                                format!(
                                    "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                                ),
                            ));
                        }
                    }
                }
            }
        }

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(upper_entries) = upper.read_dir(path) {
                directory_exists = true;
                for entry in upper_entries {
                    if entry == "." || entry == ".." {
                        continue;
                    }
                    entries.insert(entry);
                    if entries.len() > max_entries {
                        return Err(VfsError::new(
                            "ENOMEM",
                            format!(
                                "directory listing for '{path}' exceeds configured limit of {max_entries} entries"
                            ),
                        ));
                    }
                }
            }
        }

        if !directory_exists {
            return Err(Self::directory_not_found(path));
        }

        Ok(entries.into_iter().collect())
    }

    fn read_dir_with_types(&mut self, path: &str) -> VfsResult<Vec<VirtualDirEntry>> {
        if self.is_whited_out(path) {
            return Err(Self::directory_not_found(path));
        }

        let normalized = Self::normalized(path);
        let mut directory_exists = false;
        let mut entries = Vec::<VirtualDirEntry>::new();
        let mut seen = BTreeSet::<String>::new();
        let whiteouts = self.whiteouts.clone();

        for lower in self.lowers.iter_mut().rev() {
            if let Ok(lower_entries) = lower.read_dir_with_types(path) {
                directory_exists = true;
                for entry in lower_entries {
                    if entry.name == "." || entry.name == ".." {
                        continue;
                    }
                    let child_path = if normalized == "/" {
                        format!("/{}", entry.name)
                    } else {
                        format!("{normalized}/{}", entry.name)
                    };
                    if whiteouts.contains(&Self::normalized(&child_path))
                        || seen.contains(&entry.name)
                    {
                        continue;
                    }
                    seen.insert(entry.name.clone());
                    entries.push(entry);
                }
            }
        }

        if let Some(upper) = self.upper.as_mut() {
            if let Ok(upper_entries) = upper.read_dir_with_types(path) {
                directory_exists = true;
                for entry in upper_entries {
                    if entry.name == "." || entry.name == ".." {
                        continue;
                    }
                    if let Some(index) = entries
                        .iter()
                        .position(|existing| existing.name == entry.name)
                    {
                        entries[index] = entry;
                    } else {
                        seen.insert(entry.name.clone());
                        entries.push(entry);
                    }
                }
            }
        }

        if !directory_exists {
            return Err(Self::directory_not_found(path));
        }

        Ok(entries)
    }

    fn write_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.remove_whiteout(path);
        if self.find_lower_by_entry(path).is_some() {
            self.copy_up_path(path)?;
        } else {
            self.ensure_ancestor_directories_in_upper(path)?;
        }
        self.writable_upper(path)?.write_file(path, content.into())
    }

    fn create_file_exclusive(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<()> {
        self.remove_whiteout(path);
        if self.path_exists_in_merged_view(path) {
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?
            .create_file_exclusive(path, content.into())
    }

    fn append_file(&mut self, path: &str, content: impl Into<Vec<u8>>) -> VfsResult<u64> {
        self.remove_whiteout(path);
        if self.find_lower_by_entry(path).is_some() {
            self.copy_up_path(path)?;
        } else {
            self.ensure_ancestor_directories_in_upper(path)?;
        }
        self.writable_upper(path)?.append_file(path, content.into())
    }

    fn create_dir(&mut self, path: &str) -> VfsResult<()> {
        self.remove_whiteout(path);
        if self.path_exists_in_merged_view(path) {
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?.create_dir(path)
    }

    fn mkdir(&mut self, path: &str, recursive: bool) -> VfsResult<()> {
        self.remove_whiteout(path);
        if self.path_exists_in_merged_view(path) {
            let stat = self.merged_lstat(path)?;
            if recursive && stat.is_directory && !stat.is_symbolic_link {
                return Ok(());
            }
            return Err(Self::already_exists(path));
        }
        self.ensure_ancestor_directories_in_upper(path)?;
        self.writable_upper(path)?.mkdir(path, recursive)
    }

    fn exists(&self, path: &str) -> bool {
        self.path_exists_in_merged_view(path)
    }

    fn stat(&mut self, path: &str) -> VfsResult<VirtualStat> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            return self
                .upper
                .as_mut()
                .expect("upper must exist when path exists")
                .stat(path);
        }
        let Some(index) = self.find_lower_by_exists(path) else {
            return Err(Self::entry_not_found(path));
        };
        self.lowers[index].stat(path)
    }

    fn remove_file(&mut self, path: &str) -> VfsResult<()> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        let lower_exists = self.find_lower_by_exists(path).is_some();
        let upper_exists = self.exists_in_upper(path);
        if !lower_exists && !upper_exists {
            return Err(Self::entry_not_found(path));
        }
        if upper_exists {
            self.writable_upper(path)?.remove_file(path)?;
        } else {
            self.writable_upper(path)?;
        }
        self.add_whiteout(path);
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> VfsResult<()> {
        let normalized = Self::normalized(path);
        if normalized == "/" {
            return Err(VfsError::permission_denied("rmdir", path));
        }

        let stat = match self.merged_lstat(path) {
            Ok(stat) => stat,
            Err(error) if error.code() == "ENOENT" => return Err(Self::directory_not_found(path)),
            Err(error) => return Err(error),
        };

        if !stat.is_directory || stat.is_symbolic_link {
            return Err(Self::not_directory(path));
        }

        if !self.read_dir(path)?.is_empty() {
            return Err(Self::not_empty(path));
        }

        let lower_exists = self.find_lower_by_entry(path).is_some();
        let upper_exists = self.has_entry_in_upper(path);
        if upper_exists {
            self.writable_upper(path)?.remove_dir(&normalized)?;
        } else {
            self.writable_upper(path)?;
        }
        if lower_exists {
            self.add_whiteout(path);
        } else {
            self.remove_whiteout(path);
        }
        Ok(())
    }

    fn rename(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        let old_normalized = Self::normalized(old_path);
        let new_normalized = Self::normalized(new_path);

        if old_normalized == "/" {
            return Err(VfsError::permission_denied("rename", old_path));
        }

        if old_normalized == new_normalized {
            return Ok(());
        }

        let source_stat = self.merged_lstat(old_path)?;
        if source_stat.is_directory && new_normalized.starts_with(&(old_normalized.clone() + "/")) {
            return Err(VfsError::new(
                "EINVAL",
                format!(
                    "cannot move '{}' into its own descendant '{}'",
                    old_path, new_path
                ),
            ));
        }

        let mut snapshot_entries = Vec::new();
        self.collect_snapshot_entries(&old_normalized, &mut snapshot_entries)?;
        self.remove_existing_destination(&new_normalized)?;
        self.materialize_snapshot_entries(&old_normalized, &new_normalized, &snapshot_entries)?;
        self.remove_snapshot_entries(&snapshot_entries)
    }

    fn realpath(&self, path: &str) -> VfsResult<String> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            return self
                .upper
                .as_ref()
                .expect("upper must exist when path exists")
                .realpath(path);
        }
        let Some(index) = self.find_lower_by_exists(path) else {
            return Err(Self::entry_not_found(path));
        };
        self.lowers[index].realpath(path)
    }

    fn symlink(&mut self, target: &str, link_path: &str) -> VfsResult<()> {
        self.remove_whiteout(link_path);
        self.ensure_ancestor_directories_in_upper(link_path)?;
        self.writable_upper(link_path)?.symlink(target, link_path)
    }

    fn read_link(&self, path: &str) -> VfsResult<String> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.has_entry_in_upper(path) {
            return self
                .upper
                .as_ref()
                .expect("upper must exist when path exists")
                .read_link(path);
        }
        let Some((index, _)) = self.find_lower_by_entry(path) else {
            return Err(Self::entry_not_found(path));
        };
        self.lowers[index].read_link(path)
    }

    fn lstat(&self, path: &str) -> VfsResult<VirtualStat> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.has_entry_in_upper(path) {
            return self
                .upper
                .as_ref()
                .expect("upper must exist when path exists")
                .lstat(path);
        }
        self.find_lower_by_entry(path)
            .map(|(_, stat)| stat)
            .ok_or_else(|| Self::entry_not_found(path))
    }

    fn link(&mut self, old_path: &str, new_path: &str) -> VfsResult<()> {
        self.remove_whiteout(new_path);
        self.copy_up_path(old_path)?;
        self.ensure_ancestor_directories_in_upper(new_path)?;
        self.writable_upper(new_path)?.link(old_path, new_path)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> VfsResult<()> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.chmod(path, mode)
    }

    fn chown(&mut self, path: &str, uid: u32, gid: u32) -> VfsResult<()> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.chown(path, uid, gid)
    }

    fn utimes(&mut self, path: &str, atime_ms: u64, mtime_ms: u64) -> VfsResult<()> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.utimes(path, atime_ms, mtime_ms)
    }

    fn truncate(&mut self, path: &str, length: u64) -> VfsResult<()> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if !self.exists_in_upper(path) {
            self.copy_up_path(path)?;
        }
        self.writable_upper(path)?.truncate(path, length)
    }

    fn pread(&mut self, path: &str, offset: u64, length: usize) -> VfsResult<Vec<u8>> {
        if self.is_whited_out(path) {
            return Err(Self::entry_not_found(path));
        }
        if self.exists_in_upper(path) {
            return self
                .upper
                .as_mut()
                .expect("upper must exist when path exists")
                .pread(path, offset, length);
        }
        let Some(index) = self.find_lower_by_exists(path) else {
            return Err(Self::entry_not_found(path));
        };
        self.lowers[index].pread(path, offset, length)
    }
}
